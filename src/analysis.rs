//! Per-frame measurements: exposure, neutral color, sharpness and translation.

use anyhow::{bail, Result};
use image::imageops::FilterType;
use image::{GrayImage, RgbImage};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use serde::{Deserialize, Serialize};

use crate::color::srgb_to_linear;

/// Long edge used for sharpness and translation analysis.
pub const PREVIEW_MAX_DIM: u32 = 1024;
/// Maximum number of sampled pixels used for color/exposure statistics.
const MAX_COLOR_SAMPLES: u64 = 220_000;
/// A frame below this Laplacian variance is considered too blurred to stabilize.
pub const MIN_SHARPNESS: f64 = 0.0008;
/// Minimum phase-correlation peak-to-sidelobe ratio accepted as a valid motion.
pub const MIN_MOTION_CONFIDENCE: f64 = 3.0;

/// Quantified properties of one decoded frame, persisted in the TOML cache.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageAnalysis {
    pub width: u32,
    pub height: u32,
    /// Mean linear-light red, used for gray-world neutral estimation.
    pub mean_r: f64,
    pub mean_g: f64,
    pub mean_b: f64,
    /// Median sRGB luminance in 0..=1.
    pub median_luma: f64,
    pub p05_luma: f64,
    pub p95_luma: f64,
    pub highlight_fraction: f64,
    /// Variance of the Laplacian on the analysis preview.
    pub sharpness: f64,
}

impl ImageAnalysis {
    /// Rejects values that could only result from a corrupt or partial cache.
    pub fn is_sane(&self) -> bool {
        self.width > 0
            && self.height > 0
            && [
                self.mean_r,
                self.mean_g,
                self.mean_b,
                self.median_luma,
                self.p05_luma,
                self.p95_luma,
                self.highlight_fraction,
                self.sharpness,
            ]
            .iter()
            .all(|value| value.is_finite())
            && [self.mean_r, self.mean_g, self.mean_b]
                .iter()
                .all(|value| *value >= 0.0)
            && (0.0..=1.0).contains(&self.median_luma)
            && (0.0..=1.0).contains(&self.p05_luma)
            && (0.0..=1.0).contains(&self.p95_luma)
            && (0.0..=1.0).contains(&self.highlight_fraction)
            && self.sharpness >= 0.0
    }
}

/// Estimated translation of the current frame's content relative to the
/// previous frame, expressed in full-resolution pixels.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MotionEstimate {
    pub dx: f64,
    pub dy: f64,
    pub confidence: f64,
    pub valid: bool,
}

impl MotionEstimate {
    pub fn zero() -> Self {
        Self {
            dx: 0.0,
            dy: 0.0,
            confidence: 1_000_000.0,
            valid: true,
        }
    }

    /// Rejects non-finite or impossible values from a damaged cache.
    pub fn is_sane(&self) -> bool {
        self.dx.is_finite()
            && self.dy.is_finite()
            && self.confidence.is_finite()
            && self.confidence >= 0.0
    }
}

/// Single-channel analysis image used for FFT alignment.
#[derive(Clone, Debug)]
pub struct GrayPreview {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
}

/// Analyzes a decoded frame and returns both metrics and the reusable preview.
pub fn analyze_image(image: &RgbImage) -> Result<(ImageAnalysis, GrayPreview)> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        bail!("cannot analyze a zero-sized image");
    }
    let preview = make_preview(image);
    let sharpness = laplacian_variance(&preview);

    let total_pixels = u64::from(width) * u64::from(height);
    let step = ((total_pixels as f64 / MAX_COLOR_SAMPLES as f64)
        .sqrt()
        .ceil() as u32)
        .max(1);
    let mut histogram = [0_u64; 256];
    let mut histogram_total = 0_u64;
    let mut highlight_count = 0_u64;
    let mut sum_r = 0.0_f64;
    let mut sum_g = 0.0_f64;
    let mut sum_b = 0.0_f64;
    let mut weight_sum = 0.0_f64;

    for y in (0..height).step_by(step as usize) {
        for x in (0..width).step_by(step as usize) {
            let pixel = image.get_pixel(x, y).0;
            let red = pixel[0] as f32 / 255.0;
            let green = pixel[1] as f32 / 255.0;
            let blue = pixel[2] as f32 / 255.0;
            let luma = 0.2126 * red + 0.7152 * green + 0.0722 * blue;
            let bin = (luma.clamp(0.0, 1.0) * 255.0).round() as usize;
            histogram[bin] += 1;
            histogram_total += 1;
            if luma >= 250.0 / 255.0 {
                highlight_count += 1;
            }

            // Exclude deep shadows and clipped highlights from the neutral
            // estimate; they carry little reliable chromaticity information.
            if (0.025..=0.985).contains(&luma) {
                let weight = 1.0 - (luma - 0.45).abs() * 0.65;
                let weight = weight.clamp(0.15, 1.0) as f64;
                sum_r += srgb_to_linear(pixel[0]) as f64 * weight;
                sum_g += srgb_to_linear(pixel[1]) as f64 * weight;
                sum_b += srgb_to_linear(pixel[2]) as f64 * weight;
                weight_sum += weight;
            }
        }
    }

    if histogram_total == 0 {
        bail!("image sampling produced no pixels");
    }
    if weight_sum < 1.0 {
        // An all-black or all-white frame has no usable neutral estimate. Use
        // equal channels so later stages do not amplify noise.
        weight_sum = 1.0;
        sum_r = 0.5;
        sum_g = 0.5;
        sum_b = 0.5;
    }

    Ok((
        ImageAnalysis {
            width,
            height,
            mean_r: sum_r / weight_sum,
            mean_g: sum_g / weight_sum,
            mean_b: sum_b / weight_sum,
            median_luma: percentile(&histogram, histogram_total, 0.50),
            p05_luma: percentile(&histogram, histogram_total, 0.05),
            p95_luma: percentile(&histogram, histogram_total, 0.95),
            highlight_fraction: highlight_count as f64 / histogram_total as f64,
            sharpness,
        },
        preview,
    ))
}

/// Creates the grayscale preview retained during the analysis pass.
pub fn make_preview(image: &RgbImage) -> GrayPreview {
    let gray = image::imageops::grayscale(image);
    let (width, height) = gray.dimensions();
    let long_edge = width.max(height);
    let resized = if long_edge > PREVIEW_MAX_DIM {
        let scale = PREVIEW_MAX_DIM as f64 / long_edge as f64;
        let new_width = ((width as f64 * scale).round() as u32).max(1);
        let new_height = ((height as f64 * scale).round() as u32).max(1);
        image::imageops::resize(&gray, new_width, new_height, FilterType::Triangle)
    } else {
        gray
    };
    preview_from_gray(&resized)
}

fn preview_from_gray(gray: &GrayImage) -> GrayPreview {
    let (width, height) = gray.dimensions();
    let data = gray
        .pixels()
        .map(|pixel| pixel.0[0] as f32 / 255.0)
        .collect();
    GrayPreview {
        width,
        height,
        data,
    }
}

/// Computes the variance of the 4-neighbor Laplacian over the central 90%.
fn laplacian_variance(preview: &GrayPreview) -> f64 {
    let width = preview.width as usize;
    let height = preview.height as usize;
    if width < 3 || height < 3 {
        return 0.0;
    }
    let margin_x = width / 20;
    let margin_y = height / 20;
    let start_x = margin_x.max(1);
    let end_x = (width - margin_x).min(width - 1);
    let start_y = margin_y.max(1);
    let end_y = (height - margin_y).min(height - 1);

    let mut sum = 0.0_f64;
    let mut sum_sq = 0.0_f64;
    let mut count = 0_u64;
    for y in start_y..end_y {
        for x in start_x..end_x {
            let index = y * width + x;
            let center = preview.data[index] as f64;
            let left = preview.data[index - 1] as f64;
            let right = preview.data[index + 1] as f64;
            let up = preview.data[index - width] as f64;
            let down = preview.data[index + width] as f64;
            let laplacian = 4.0 * center - left - right - up - down;
            sum += laplacian;
            sum_sq += laplacian * laplacian;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    let mean = sum / count as f64;
    (sum_sq / count as f64 - mean * mean).max(0.0)
}

/// Reads a quantile from a 256-bin luminance histogram.
fn percentile(histogram: &[u64; 256], total: u64, quantile: f64) -> f64 {
    let target = (quantile.clamp(0.0, 1.0) * total as f64).ceil() as u64;
    let mut seen = 0_u64;
    for (bin, count) in histogram.iter().enumerate() {
        seen += *count;
        if seen >= target.max(1) {
            return bin as f64 / 255.0;
        }
    }
    1.0
}

/// Estimates translation between two previews with windowed phase correlation.
///
/// The returned translation describes how far the *content* of `current` moved
/// relative to `previous`, in full-resolution pixels.
pub fn estimate_motion(
    previous: &GrayPreview,
    current: &GrayPreview,
    full_width: u32,
    full_height: u32,
) -> Result<MotionEstimate> {
    if previous.width != current.width || previous.height != current.height {
        return Ok(MotionEstimate {
            dx: 0.0,
            dy: 0.0,
            confidence: 0.0,
            valid: false,
        });
    }
    if previous.width < 16 || previous.height < 16 {
        return Ok(MotionEstimate {
            dx: 0.0,
            dy: 0.0,
            confidence: 0.0,
            valid: false,
        });
    }

    let width = previous.width as usize;
    let height = previous.height as usize;
    let size = width.max(height).next_power_of_two().max(64);
    let mut planner = FftPlanner::<f32>::new();
    let forward = planner.plan_fft_forward(size);
    let inverse = planner.plan_fft_inverse(size);
    let scratch_len = forward
        .get_inplace_scratch_len()
        .max(inverse.get_inplace_scratch_len());
    let mut scratch = vec![Complex::new(0.0_f32, 0.0_f32); scratch_len];

    let mut left = vec![Complex::new(0.0_f32, 0.0_f32); size * size];
    let mut right = vec![Complex::new(0.0_f32, 0.0_f32); size * size];
    fill_windowed(&mut left, previous, size);
    fill_windowed(&mut right, current, size);

    fft_2d(&mut left, size, &forward, &mut scratch);
    fft_2d(&mut right, size, &forward, &mut scratch);

    for (a, b) in left.iter_mut().zip(right.iter()) {
        let cross = *a * b.conj();
        let magnitude = cross.norm();
        *a = if magnitude > 1e-12 {
            cross / magnitude
        } else {
            Complex::new(0.0, 0.0)
        };
    }

    fft_2d(&mut left, size, &inverse, &mut scratch);

    let (peak_x, peak_y, peak) = find_peak(&left, size);
    let (confidence, _) = peak_confidence(&left, size, peak_x, peak_y, peak);
    if confidence < MIN_MOTION_CONFIDENCE {
        return Ok(MotionEstimate {
            dx: 0.0,
            dy: 0.0,
            confidence,
            valid: false,
        });
    }

    let sub_x = subpixel_offset(&left, size, peak_x, peak_y, true);
    let sub_y = subpixel_offset(&left, size, peak_x, peak_y, false);
    let signed_x = signed_peak(peak_x, size) + sub_x;
    let signed_y = signed_peak(peak_y, size) + sub_y;

    // A shift larger than a quarter frame is almost certainly a false peak or
    // a circular wrap; refusing it prevents catastrophic crops.
    let max_shift = (size as f64 / 4.0).max(4.0);
    if signed_x.abs() > max_shift || signed_y.abs() > max_shift {
        return Ok(MotionEstimate {
            dx: 0.0,
            dy: 0.0,
            confidence,
            valid: false,
        });
    }

    let scale_x = full_width as f64 / width as f64;
    let scale_y = full_height as f64 / height as f64;
    Ok(MotionEstimate {
        dx: -signed_x * scale_x,
        dy: -signed_y * scale_y,
        confidence,
        valid: true,
    })
}

fn fill_windowed(buffer: &mut [Complex<f32>], preview: &GrayPreview, size: usize) {
    let width = preview.width as usize;
    let height = preview.height as usize;
    let mean =
        preview.data.iter().map(|value| *value as f64).sum::<f64>() / preview.data.len() as f64;
    let denominator = (size.saturating_sub(1)).max(1) as f32;
    let mut window = vec![0.0_f32; size];
    for (index, value) in window.iter_mut().enumerate() {
        *value = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * index as f32 / denominator).cos();
    }
    for y in 0..height {
        for x in 0..width {
            let value = (preview.data[y * width + x] as f64 - mean) as f32;
            buffer[y * size + x] = Complex::new(value * window[x] * window[y], 0.0);
        }
    }
}

/// In-place 2D FFT (rows, then columns) with caller-owned scratch space.
fn fft_2d(
    buffer: &mut [Complex<f32>],
    size: usize,
    fft: &std::sync::Arc<dyn rustfft::Fft<f32>>,
    scratch: &mut [Complex<f32>],
) {
    let mut line = vec![Complex::new(0.0_f32, 0.0_f32); size];
    for y in 0..size {
        line.copy_from_slice(&buffer[y * size..(y + 1) * size]);
        fft.process_with_scratch(&mut line, scratch);
        buffer[y * size..(y + 1) * size].copy_from_slice(&line);
    }
    for x in 0..size {
        for y in 0..size {
            line[y] = buffer[y * size + x];
        }
        fft.process_with_scratch(&mut line, scratch);
        for y in 0..size {
            buffer[y * size + x] = line[y];
        }
    }
}

fn find_peak(buffer: &[Complex<f32>], size: usize) -> (usize, usize, f64) {
    let mut best = (0_usize, 0_usize, -1.0_f64);
    for y in 0..size {
        for x in 0..size {
            let value = buffer[y * size + x].norm() as f64;
            if value > best.2 {
                best = (x, y, value);
            }
        }
    }
    best
}

/// Peak-to-sidelobe ratio: how many standard deviations the peak is above the
/// correlation background. Also returns the background mean.
fn peak_confidence(
    buffer: &[Complex<f32>],
    size: usize,
    peak_x: usize,
    peak_y: usize,
    peak: f64,
) -> (f64, f64) {
    let radius = 5_isize;
    let mut sum = 0.0_f64;
    let mut sum_sq = 0.0_f64;
    let mut count = 0_u64;
    for y in 0..size {
        for x in 0..size {
            let dx = x as isize - peak_x as isize;
            let dy = y as isize - peak_y as isize;
            // Use circular distance so peaks at the array edges are masked too.
            let wrapped_x = dx.abs().min(size as isize - dx.abs());
            let wrapped_y = dy.abs().min(size as isize - dy.abs());
            if wrapped_x <= radius && wrapped_y <= radius {
                continue;
            }
            let value = buffer[y * size + x].norm() as f64;
            sum += value;
            sum_sq += value * value;
            count += 1;
        }
    }
    if count < 2 {
        return (0.0, 0.0);
    }
    let mean = sum / count as f64;
    let variance = (sum_sq / count as f64 - mean * mean).max(0.0);
    let stddev = variance.sqrt();
    let confidence = if stddev > 1e-12 {
        ((peak - mean) / stddev).min(1_000_000.0)
    } else if peak > mean * 2.0 {
        1_000_000.0
    } else {
        0.0
    };
    (confidence, mean)
}

/// Parabolic interpolation around the peak for sub-pixel translation.
fn subpixel_offset(
    buffer: &[Complex<f32>],
    size: usize,
    peak_x: usize,
    peak_y: usize,
    horizontal: bool,
) -> f64 {
    let wrap = |value: isize| -> usize { value.rem_euclid(size as isize) as usize };
    let center = buffer[peak_y * size + peak_x].norm() as f64;
    let (left, right) = if horizontal {
        (
            buffer[peak_y * size + wrap(peak_x as isize - 1)].norm() as f64,
            buffer[peak_y * size + wrap(peak_x as isize + 1)].norm() as f64,
        )
    } else {
        (
            buffer[wrap(peak_y as isize - 1) * size + peak_x].norm() as f64,
            buffer[wrap(peak_y as isize + 1) * size + peak_x].norm() as f64,
        )
    };
    let denominator = left - 2.0 * center + right;
    if denominator.abs() < 1e-12 {
        0.0
    } else {
        (0.5 * (left - right) / denominator).clamp(-0.5, 0.5)
    }
}

/// Converts an unsigned FFT peak index to a signed shift.
fn signed_peak(index: usize, size: usize) -> f64 {
    if index <= size / 2 {
        index as f64
    } else {
        index as f64 - size as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_preview(width: u32, height: u32, shift_x: i32, shift_y: i32) -> GrayPreview {
        let mut data = vec![0.0_f32; (width * height) as usize];
        for y in 0..height {
            for x in 0..width {
                let source_x = x as i32 - shift_x;
                let source_y = y as i32 - shift_y;
                if source_x >= 0
                    && source_y >= 0
                    && source_x < width as i32
                    && source_y < height as i32
                {
                    let fx = source_x as f32 * 0.173;
                    let fy = source_y as f32 * 0.097;
                    data[(y * width + x) as usize] =
                        ((fx.sin() + fy.cos() + (fx * 0.37 + fy).sin()) * 0.25 + 0.5)
                            .clamp(0.0, 1.0);
                }
            }
        }
        GrayPreview {
            width,
            height,
            data,
        }
    }

    #[test]
    fn phase_correlation_recovers_translation_sign() {
        let previous = synthetic_preview(256, 192, 0, 0);
        let current = synthetic_preview(256, 192, 6, -4);
        let motion = estimate_motion(&previous, &current, 2560, 1920).unwrap();
        assert!(motion.valid, "confidence {}", motion.confidence);
        assert!((motion.dx - 60.0).abs() < 4.0, "dx={}", motion.dx);
        assert!((motion.dy + 40.0).abs() < 4.0, "dy={}", motion.dy);
    }

    #[test]
    fn sharpness_is_higher_for_texture() {
        let textured = synthetic_preview(256, 256, 0, 0);
        let flat = GrayPreview {
            width: 256,
            height: 256,
            data: vec![0.5; 256 * 256],
        };
        assert!(laplacian_variance(&textured) > laplacian_variance(&flat));
    }
}
