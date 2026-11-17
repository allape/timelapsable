//! Applying the sequence plan and encoding output files.

use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder, RgbImage};

use crate::atomic::atomic_write;
use crate::cli::OutputFormat;
use crate::color::build_channel_lut;
use crate::plan::FramePlan;

/// Applies white balance, smoothed exposure and the manual brightness offset,
/// then crops according to the stabilization plan.
pub fn render_frame(
    mut image: RgbImage,
    plan: &FramePlan,
    brightness_percent: f64,
) -> Result<RgbImage> {
    let offset = (brightness_percent / 100.0) as f32;
    let gains = [
        plan.wb_gain[0] * plan.exposure_gain,
        plan.wb_gain[1] * plan.exposure_gain,
        plan.wb_gain[2] * plan.exposure_gain,
    ];
    let lookup = [
        build_channel_lut(gains[0], offset),
        build_channel_lut(gains[1], offset),
        build_channel_lut(gains[2], offset),
    ];

    for pixel in image.pixels_mut() {
        let source = pixel.0;
        pixel.0 = [
            lookup[0][source[0] as usize],
            lookup[1][source[1] as usize],
            lookup[2][source[2] as usize],
        ];
    }

    if let Some(crop) = plan.crop {
        let (width, height) = image.dimensions();
        if crop.x + crop.width > width || crop.y + crop.height > height {
            bail!(
                "crop rectangle {}x{}+{}+{} exceeds frame {}x{}",
                crop.width,
                crop.height,
                crop.x,
                crop.y,
                width,
                height
            );
        }
        image =
            image::imageops::crop_imm(&image, crop.x, crop.y, crop.width, crop.height).to_image();
    }

    Ok(image)
}

/// Encodes an RGB image to a temporary sibling and atomically renames it.
pub fn encode_image(path: &Path, image: &RgbImage, format: OutputFormat) -> Result<()> {
    let (width, height) = image.dimensions();
    atomic_write(path, |file| {
        let mut writer = BufWriter::new(file);
        match format {
            OutputFormat::Jpeg => {
                let encoder = JpegEncoder::new_with_quality(&mut writer, 92);
                encoder
                    .write_image(image.as_raw(), width, height, ExtendedColorType::Rgb8)
                    .map_err(std::io::Error::other)?;
            }
            OutputFormat::Png => {
                let encoder = PngEncoder::new(&mut writer);
                encoder
                    .write_image(image.as_raw(), width, height, ExtendedColorType::Rgb8)
                    .map_err(std::io::Error::other)?;
            }
        }
        writer.flush()?;
        Ok(())
    })
    .with_context(|| format!("cannot write output image '{}'", path.display()))
}
