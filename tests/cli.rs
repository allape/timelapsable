//! End-to-end tests that exercise the compiled command line binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use image::{Rgb, RgbImage};

/// Self-cleaning temporary directory.
struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "timelapsable-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_timelapsable"))
        .current_dir(root)
        .args(args)
        .output()
        .expect("failed to execute timelapsable")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn generate_frame(path: &Path, frame: usize, shift_x: i32, shift_y: i32, flat: bool) {
    let width = 360_u32;
    let height = 240_u32;
    let image = RgbImage::from_fn(width, height, |x, y| {
        if flat {
            return Rgb([90, 100, 110]);
        }
        let source_x = (x as i32 - shift_x).rem_euclid(width as i32);
        let source_y = (y as i32 - shift_y).rem_euclid(height as i32);
        let base = ((source_x * 37 + source_y * 19 + source_x * source_y) % 251) as f32 / 255.0;
        let fine = (((source_x * 11 + source_y * 7) % 17) as f32) / 255.0;
        let exposure = 0.88 + frame as f32 * 0.025;
        let warm = 0.05 * frame as f32;
        Rgb([
            ((base * 0.75 + fine * 0.25) * exposure * 255.0 + warm * 20.0).clamp(0.0, 255.0) as u8,
            ((base * 0.8 + fine * 0.2) * exposure * 255.0).clamp(0.0, 255.0) as u8,
            ((base * 0.7 + fine * 0.3) * exposure * 255.0 - warm * 15.0).clamp(0.0, 255.0) as u8,
        ])
    });
    image.save(path).unwrap();
}

fn sequence() -> (TestDir, PathBuf) {
    let dir = TestDir::new("sequence");
    let input = dir.path().join("input");
    fs::create_dir_all(&input).unwrap();
    let shakes = [(0, 0), (6, 2), (0, 0), (6, 2), (0, 0)];
    for (index, (x, y)) in shakes.iter().enumerate() {
        generate_frame(
            &input.join(format!("frame_{index:03}.png")),
            index,
            *x,
            *y,
            false,
        );
    }
    (dir, input)
}

fn output_names(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root.join(".timelapsable"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn end_to_end_writes_png_outputs_and_complete_cache() {
    let (dir, _input) = sequence();
    let output = run(
        dir.path(),
        &["-f", "input", "--format", ".png", "--brightness", "5"],
    );
    assert_success(&output);

    let names = output_names(dir.path());
    let pngs: Vec<_> = names
        .iter()
        .filter(|name| name.ends_with(".tlpb.png"))
        .collect();
    let metas: Vec<_> = names
        .iter()
        .filter(|name| name.starts_with(".meta."))
        .collect();
    assert_eq!(pngs.len(), 5, "outputs: {names:?}");
    assert_eq!(metas.len(), 5, "metadata: {names:?}");
    assert!(names.contains(&"frame_000.tlpb.png".to_string()));
    assert!(names.contains(&".meta.frame_000.toml".to_string()));

    let first = image::open(dir.path().join(".timelapsable/frame_000.tlpb.png")).unwrap();
    assert!(first.width() < 360, "stabilization should crop: {first:?}");
    assert!(
        first.height() <= 240,
        "stabilization should not enlarge: {first:?}"
    );

    let cache_text =
        fs::read_to_string(dir.path().join(".timelapsable/.meta.frame_000.toml")).unwrap();
    let cache: toml::Value = toml::from_str(&cache_text).unwrap();
    assert_eq!(cache["complete"].as_bool(), Some(true));
    assert!(cache["checksum"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(cache["analysis"]["width"].as_integer(), Some(360));
    assert!(cache["exif"].is_table());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Found 5 image(s)"));
    assert!(stdout.contains("frame_000.png"));
    assert!(stdout.contains("cache hit") || stdout.contains("analyzed and cached"));
}

#[test]
fn cache_is_reused_when_only_brightness_changes() {
    let (dir, _input) = sequence();
    let first = run(
        dir.path(),
        &["-f", "input", "--format", ".png", "-s", "false"],
    );
    assert_success(&first);
    let meta_path = dir.path().join(".timelapsable/.meta.frame_000.toml");
    let before = fs::metadata(&meta_path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));

    let second = run(
        dir.path(),
        &[
            "-f", "input", "--format", ".png", "-s", "false", "-b", "12", "-w", "5600",
        ],
    );
    assert_success(&second);
    let after = fs::metadata(&meta_path).unwrap().modified().unwrap();
    assert_eq!(before, after, "cache should not be rewritten on a full hit");
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(stdout.contains("5 cache hit(s)"), "stdout: {stdout}");
}

#[test]
fn blurry_sequence_uses_special_exit_code_and_can_be_overridden() {
    let dir = TestDir::new("blur");
    let input = dir.path().join("input");
    fs::create_dir_all(&input).unwrap();
    for index in 0..3 {
        generate_frame(
            &input.join(format!("blur_{index:03}.png")),
            index,
            0,
            0,
            true,
        );
    }

    let failed = run(dir.path(), &["-f", "input", "--format", ".png"]);
    assert_eq!(failed.status.code(), Some(4));
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        stderr.contains("stabilization failed") || stderr.contains("too blurred"),
        "stderr: {stderr}"
    );

    let succeeded = run(
        dir.path(),
        &["-f", "input", "--format", ".png", "-s", "false"],
    );
    assert_success(&succeeded);
}

#[test]
fn brightness_extremes_produce_black_and_white() {
    let (dir, _input) = sequence();
    let white = run(
        dir.path(),
        &[
            "-f", "input", "--format", ".png", "-s", "false", "-b", "100",
        ],
    );
    assert_success(&white);
    let image = image::open(dir.path().join(".timelapsable/frame_002.tlpb.png")).unwrap();
    assert!(image
        .to_rgb8()
        .pixels()
        .all(|pixel| pixel.0 == [255, 255, 255]));

    let black = run(
        dir.path(),
        &[
            "-f", "input", "--format", ".png", "-s", "false", "-b", "-100",
        ],
    );
    assert_success(&black);
    let image = image::open(dir.path().join(".timelapsable/frame_002.tlpb.png")).unwrap();
    assert!(image.to_rgb8().pixels().all(|pixel| pixel.0 == [0, 0, 0]));
}

#[test]
fn invalid_values_are_usage_errors() {
    let dir = TestDir::new("usage");
    let input = dir.path().join("input");
    fs::create_dir_all(&input).unwrap();
    generate_frame(&input.join("a.png"), 0, 0, 0, false);

    let bad_brightness = run(dir.path(), &["-f", "input", "-b", "150"]);
    assert_eq!(bad_brightness.status.code(), Some(2));
    let bad_wb = run(dir.path(), &["-f", "input", "-w", "100"]);
    assert_eq!(bad_wb.status.code(), Some(2));
    let bad_format = run(dir.path(), &["-f", "input", "--format", ".tif"]);
    assert_eq!(bad_format.status.code(), Some(2));
}

#[test]
fn default_jpeg_output_and_high_brightness_warning() {
    let (dir, _input) = sequence();
    let output = run(dir.path(), &["-f", "input", "-s", "false", "-b", "95"]);
    assert_success(&output);
    assert!(dir
        .path()
        .join(".timelapsable/frame_000.tlpb.jpg")
        .is_file());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("recommended range"), "stderr: {stderr}");
}

#[test]
fn truncated_cache_is_reported_and_regenerated() {
    let (dir, _input) = sequence();
    let first = run(
        dir.path(),
        &["-f", "input", "--format", ".png", "-s", "false"],
    );
    assert_success(&first);
    let meta_path = dir.path().join(".timelapsable/.meta.frame_000.toml");
    fs::write(&meta_path, "cache_version = 1\ncomplete = true\n").unwrap();

    let second = run(
        dir.path(),
        &["-f", "input", "--format", ".png", "-s", "false"],
    );
    assert_success(&second);
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("incomplete or invalid"), "stderr: {stderr}");
    let cache: toml::Value = toml::from_str(&fs::read_to_string(&meta_path).unwrap()).unwrap();
    assert_eq!(cache["complete"].as_bool(), Some(true));
    assert!(cache["checksum"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
}

fn generate_flicker_frame(path: &Path, frame: usize) {
    let exposure = [0.55_f32, 1.0, 0.6, 0.96, 0.55, 1.0][frame % 6];
    let red_mul = [1.25_f32, 0.9, 1.2, 0.95, 1.3, 0.9][frame % 6];
    let blue_mul = [0.8_f32, 1.2, 0.85, 1.15, 0.8, 1.25][frame % 6];
    let width = 220_u32;
    let height = 160_u32;
    let image = RgbImage::from_fn(width, height, |x, y| {
        let texture = ((x * 29 + y * 13 + x * y) % 199) as f32 / 198.0;
        let base = 0.18 + texture * 0.52;
        Rgb([
            (base * exposure * red_mul * 255.0).clamp(0.0, 255.0) as u8,
            (base * exposure * 255.0).clamp(0.0, 255.0) as u8,
            (base * exposure * blue_mul * 255.0).clamp(0.0, 255.0) as u8,
        ])
    });
    image.save(path).unwrap();
}

fn image_stats(path: &Path) -> (f64, f64, f64) {
    let image = image::open(path).unwrap().to_rgb8();
    let mut total = 0.0_f64;
    let mut red = 0.0_f64;
    let mut green = 0.0_f64;
    let mut blue = 0.0_f64;
    for pixel in image.pixels() {
        let r = pixel.0[0] as f64 / 255.0;
        let g = pixel.0[1] as f64 / 255.0;
        let b = pixel.0[2] as f64 / 255.0;
        total += 0.2126 * r + 0.7152 * g + 0.0722 * b;
        red += to_linear(r);
        green += to_linear(g);
        blue += to_linear(b);
    }
    let count = (image.width() * image.height()) as f64;
    let green_mean = (green / count).max(1e-9);
    (
        total / count,
        (red / count) / green_mean,
        (blue / count) / green_mean,
    )
}

fn to_linear(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn stddev(values: &[f64]) -> f64 {
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (values
        .iter()
        .map(|value| (value - mean) * (value - mean))
        .sum::<f64>()
        / values.len() as f64)
        .sqrt()
}

#[test]
fn automatic_matching_reduces_brightness_and_color_flicker() {
    let dir = TestDir::new("flicker");
    let input = dir.path().join("input");
    fs::create_dir_all(&input).unwrap();
    for frame in 0..6 {
        generate_flicker_frame(&input.join(format!("flicker_{frame:03}.png")), frame);
    }
    let output = run(
        dir.path(),
        &["-f", "input", "--format", ".png", "-s", "false"],
    );
    assert_success(&output);

    let mut input_luma = Vec::new();
    let mut output_luma = Vec::new();
    let mut input_red = Vec::new();
    let mut output_red = Vec::new();
    let mut input_blue = Vec::new();
    let mut output_blue = Vec::new();
    for frame in 0..6 {
        let name = format!("flicker_{frame:03}");
        let (il, ir, ib) = image_stats(&input.join(format!("{name}.png")));
        let (ol, or, ob) = image_stats(&dir.path().join(format!(".timelapsable/{name}.tlpb.png")));
        input_luma.push(il);
        output_luma.push(ol);
        input_red.push(ir);
        output_red.push(or);
        input_blue.push(ib);
        output_blue.push(ob);
    }
    assert!(
        stddev(&output_luma) < stddev(&input_luma) * 0.5,
        "luma flicker: input {:?} output {:?}",
        input_luma,
        output_luma
    );
    assert!(
        stddev(&output_red) < stddev(&input_red) * 0.8,
        "red flicker: input {:?} output {:?}",
        input_red,
        output_red
    );
    assert!(
        stddev(&output_blue) < stddev(&input_blue) * 0.8,
        "blue flicker: input {:?} output {:?}",
        input_blue,
        output_blue
    );
}
