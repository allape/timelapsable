//! High-level orchestration: scan, cached analysis, planning and rendering.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::analysis::{self, GrayPreview};
use crate::cache::{CacheLoad, FrameCache};
use crate::cli::{self, Cli};
use crate::color::WhiteBalance;
use crate::decode;
use crate::error::{StabilizationError, UsageError};
use crate::metadata;
use crate::plan::{build_plan, SequencePlan};
use crate::render::{encode_image, render_frame};
use crate::scan::{self, InputImage};
use crate::source::SourceId;

/// Runs the complete timelapse pipeline.
pub fn run(cli: Cli) -> Result<()> {
    cli::validate(&cli)?;
    let white_balance = parse_white_balance(cli.white_balance.as_deref())?;
    let brightness = cli.brightness.unwrap_or(0.0);

    let cwd = std::env::current_dir().context("cannot determine current working directory")?;
    let folder = resolve_folder(&cwd, &cli.folder)?;
    let output_dir = folder.join(".timelapsable");

    let images = scan::scan_folder(&folder)?;
    if images.is_empty() {
        bail!(
            "no supported images (ARW, JPG, JPEG, PNG) found in '{}'",
            folder.display()
        );
    }

    print_scan(
        &folder,
        &output_dir,
        &images,
        &cli,
        white_balance,
        brightness,
    );
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("cannot create output directory '{}'", output_dir.display()))?;

    let caches = analyze_sequence(&images, &output_dir, cli.steadicam)?;
    let plan = build_plan(&images, &caches, cli.steadicam, white_balance)?;
    print_plan(&plan);

    render_sequence(&images, &caches, &plan, &output_dir, cli.format, brightness)?;

    println!(
        "Done: {} frame(s) written to {}",
        images.len(),
        output_dir.display()
    );
    Ok(())
}

fn parse_white_balance(value: Option<&str>) -> Result<WhiteBalance> {
    let value = value.unwrap_or("auto");
    value
        .parse()
        .map_err(|message: String| UsageError(message).into())
}

fn resolve_folder(cwd: &Path, folder: &Path) -> Result<PathBuf> {
    let candidate = if folder.is_absolute() {
        folder.to_path_buf()
    } else {
        cwd.join(folder)
    };
    fs::canonicalize(&candidate)
        .with_context(|| format!("cannot resolve input folder '{}'", candidate.display()))
}

fn print_scan(
    folder: &Path,
    output_dir: &Path,
    images: &[InputImage],
    cli: &Cli,
    white_balance: WhiteBalance,
    brightness: f64,
) {
    println!("Input folder: {}", folder.display());
    println!("Found {} image(s) in alphabetical order:", images.len());
    for (index, image) in images.iter().enumerate() {
        println!("  {:>4}. {}", index + 1, image.file_name);
    }
    println!("Output folder: {}", output_dir.display());
    println!("Output format: {}", cli.format.extension());
    println!(
        "Stabilization: {}",
        if cli.steadicam { "enabled" } else { "disabled" }
    );
    match white_balance {
        WhiteBalance::Auto => println!("White balance: auto"),
        WhiteBalance::Temperature(kelvin) => {
            println!("White balance: {kelvin:.0} K (6500 K = neutral)")
        }
    }
    if brightness == 0.0 {
        println!("Brightness: automatic exposure matching, no manual offset");
    } else {
        println!(
            "Brightness: automatic exposure matching, manual offset {:+.1}%",
            brightness
        );
    }
}

/// Runs the analysis pass, reusing complete caches without decoding images.
fn analyze_sequence(
    images: &[InputImage],
    output_dir: &Path,
    steadicam: bool,
) -> Result<Vec<FrameCache>> {
    println!("Analyzing {} frame(s)...", images.len());
    let mut caches = Vec::with_capacity(images.len());
    let mut previous_preview: Option<GrayPreview> = None;
    let mut cache_hits = 0_usize;
    let mut analyzed = 0_usize;

    for (index, image) in images.iter().enumerate() {
        let expected_prev = index
            .checked_sub(1)
            .map(|previous| images[previous].source.clone());
        let cache_path = output_dir.join(image.meta_name());
        let load = FrameCache::load(&cache_path, &image.source, expected_prev.as_ref());

        let (mut cache, current_preview) = match load {
            CacheLoad::Hit(cache) if !steadicam || index == 0 || cache.motion.valid => {
                cache_hits += 1;
                println!(
                    "[{}/{}] {}: cache hit",
                    index + 1,
                    images.len(),
                    image.file_name
                );
                caches.push(*cache);
                // No preview is retained for a cache hit. If the next frame
                // misses, it lazily decodes this predecessor once.
                previous_preview = None;
                continue;
            }
            CacheLoad::Hit(_) => {
                eprintln!(
                    "warning: cached motion for '{}' is unusable; re-analyzing this frame",
                    image.file_name
                );
                analyze_one(image, expected_prev.clone())?
            }
            CacheLoad::Miss { reason } => {
                if reason != "no cache file" {
                    eprintln!(
                        "warning: cache for '{}' is incomplete or invalid ({}); re-analyzing",
                        image.file_name, reason
                    );
                }
                analyze_one(image, expected_prev.clone())?
            }
        };

        if index > 0 {
            let previous = match previous_preview.take() {
                Some(preview) => preview,
                None => decode::decode(&images[index - 1].path)
                    .with_context(|| {
                        format!(
                            "cannot decode previous frame '{}' for alignment",
                            images[index - 1].file_name
                        )
                    })
                    .map(|decoded| analysis::make_preview(&decoded.image))?,
            };
            let motion = analysis::estimate_motion(
                &previous,
                &current_preview,
                cache.analysis.width,
                cache.analysis.height,
            )?;
            if steadicam && !motion.valid {
                return Err(StabilizationError(format!(
                    "frame '{}' is too blurred or too low-detail to align \
                     (phase-correlation confidence {:.2} < {:.2}); \
                     use -s false to process without stabilization",
                    image.file_name,
                    motion.confidence,
                    analysis::MIN_MOTION_CONFIDENCE
                ))
                .into());
            }
            cache.prev = expected_prev;
            cache.motion = motion;
        } else {
            cache.prev = None;
            cache.motion = analysis::MotionEstimate::zero();
        }

        cache.write(&cache_path)?;
        analyzed += 1;
        println!(
            "[{}/{}] {}: analyzed and cached",
            index + 1,
            images.len(),
            image.file_name
        );
        previous_preview = Some(current_preview);
        caches.push(cache);
    }

    println!("Analysis complete: {cache_hits} cache hit(s), {analyzed} frame(s) analyzed.");
    Ok(caches)
}

/// Decodes and analyzes one cache-miss frame, returning its reusable preview.
fn analyze_one(image: &InputImage, prev: Option<SourceId>) -> Result<(FrameCache, GrayPreview)> {
    let decoded = decode::decode(&image.path)?;
    let mut exif = metadata::read_exif(&image.path)?;
    if let Some(error) = &exif.read_error {
        eprintln!(
            "warning: could not parse EXIF metadata in '{}': {}",
            image.file_name, error
        );
    }
    exif.fill_camera(decoded.raw_make.as_deref(), decoded.raw_model.as_deref());
    let (analysis, preview) = analysis::analyze_image(&decoded.image)?;
    Ok((
        FrameCache::new(
            image.source.clone(),
            prev,
            analysis,
            analysis::MotionEstimate::zero(),
            exif,
        ),
        preview,
    ))
}

/// Renders all planned frames and writes them to the output folder.
fn render_sequence(
    images: &[InputImage],
    caches: &[FrameCache],
    plan: &SequencePlan,
    output_dir: &Path,
    format: cli::OutputFormat,
    brightness: f64,
) -> Result<()> {
    println!("Rendering {} frame(s)...", images.len());
    for (index, ((image, cache), frame_plan)) in images
        .iter()
        .zip(caches.iter())
        .zip(plan.frames.iter())
        .enumerate()
    {
        let current_source = SourceId::from_path(&image.path)?;
        if current_source != image.source {
            bail!(
                "input file '{}' changed after analysis; rerun the command",
                image.file_name
            );
        }
        let decoded = decode::decode(&image.path)?;
        let (width, height) = decoded.dimensions();
        if width != cache.analysis.width || height != cache.analysis.height {
            bail!(
                "frame '{}' changed dimensions from {}x{} to {}x{} during processing",
                image.file_name,
                cache.analysis.width,
                cache.analysis.height,
                width,
                height
            );
        }
        let rendered = render_frame(decoded.image, frame_plan, brightness)?;
        let output_path = output_dir.join(image.output_name(format));
        encode_image(&output_path, &rendered, format)?;
        println!(
            "[{}/{}] {} -> {} ({}x{})",
            index + 1,
            images.len(),
            image.file_name,
            output_path
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default(),
            rendered.width(),
            rendered.height()
        );
    }
    Ok(())
}

fn print_plan(plan: &SequencePlan) {
    if plan.stabilization_enabled {
        if plan.cropped {
            println!(
                "Stabilization crop: {}x{} output (crop-based shake removal)",
                plan.output_width, plan.output_height
            );
        } else {
            println!(
                "Stabilization crop: none needed ({}x{})",
                plan.output_width, plan.output_height
            );
        }
    }
    println!(
        "Exposure matching: mean linear gain {:.3}",
        plan.mean_exposure_gain()
    );
}
