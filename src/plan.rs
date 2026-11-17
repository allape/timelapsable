//! Turning cached measurements into a global exposure, white-balance and crop plan.

use anyhow::Result;

use crate::analysis::MIN_SHARPNESS;
use crate::cache::FrameCache;
use crate::color::WhiteBalance;
use crate::error::StabilizationError;
use crate::scan::InputImage;

/// Integer crop rectangle applied to one rendered frame.
#[derive(Clone, Copy, Debug)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Per-frame parameters after temporal smoothing.
#[derive(Clone, Debug)]
pub struct FramePlan {
    /// Exposure normalization in linear light.
    pub exposure_gain: f32,
    /// White-balance multipliers in linear light, already luminance-normalized.
    pub wb_gain: [f32; 3],
    pub crop: Option<CropRect>,
}

/// Plan shared by every frame in the sequence.
#[derive(Clone, Debug)]
pub struct SequencePlan {
    pub frames: Vec<FramePlan>,
    pub output_width: u32,
    pub output_height: u32,
    pub cropped: bool,
    pub stabilization_enabled: bool,
}

impl SequencePlan {
    /// Representative exposure adjustment for the log line.
    pub fn mean_exposure_gain(&self) -> f32 {
        if self.frames.is_empty() {
            return 1.0;
        }
        self.frames
            .iter()
            .map(|frame| frame.exposure_gain)
            .sum::<f32>()
            / self.frames.len() as f32
    }
}

/// Builds the sequence plan. Stabilization failures use the dedicated error
/// type so `main` can return exit status 4.
pub fn build_plan(
    images: &[InputImage],
    caches: &[FrameCache],
    steadicam: bool,
    white_balance: WhiteBalance,
) -> Result<SequencePlan> {
    if images.len() != caches.len() {
        anyhow::bail!("internal error: image and cache counts differ");
    }
    if images.is_empty() {
        anyhow::bail!("cannot plan an empty sequence");
    }

    let exposure_gains = exposure_gains(caches);
    let wb_gains = white_balance_gains(caches, white_balance);

    let (crops, output_width, output_height, cropped) = if steadicam {
        stabilization_crops(images, caches)?
    } else {
        let first = &caches[0].analysis;
        (vec![None; caches.len()], first.width, first.height, false)
    };

    let frames = caches
        .iter()
        .enumerate()
        .map(|(index, _)| FramePlan {
            exposure_gain: exposure_gains[index],
            wb_gain: wb_gains[index],
            crop: crops[index],
        })
        .collect();

    Ok(SequencePlan {
        frames,
        output_width,
        output_height,
        cropped,
        stabilization_enabled: steadicam,
    })
}

/// Smooths median luminance and derives exposure gains that remove flicker
/// while preserving the sequence's gradual day-to-night trend.
fn exposure_gains(caches: &[FrameCache]) -> Vec<f32> {
    let medians: Vec<f64> = caches
        .iter()
        .map(|cache| cache.analysis.median_luma)
        .collect();
    let smoothed = smooth_series(&medians, 2, 6);
    medians
        .iter()
        .zip(smoothed.iter())
        .map(|(current, target)| {
            let current_linear = srgb_to_linear_f64(*current).max(1e-5);
            let target_linear = srgb_to_linear_f64(*target).max(1e-5);
            (target_linear / current_linear).clamp(0.25, 4.0) as f32
        })
        .collect()
}

/// Builds a temporally smooth white-balance target from observed gray-world
/// ratios, then derives the per-frame gains needed to reach that target.
fn white_balance_gains(caches: &[FrameCache], white_balance: WhiteBalance) -> Vec<[f32; 3]> {
    // Measure the input's neutral error in log space. The target is a smoothed
    // version of that error, so the gain removes rapid flicker while preserving
    // a gradual color trend. Smoothing the *gains* directly would leave the
    // original per-frame casts intact on short sequences.
    let mut observed_r = Vec::with_capacity(caches.len());
    let mut observed_b = Vec::with_capacity(caches.len());
    for cache in caches {
        let analysis = &cache.analysis;
        let log_r = (analysis.mean_r.max(1e-6) / analysis.mean_g.max(1e-6)).ln();
        let log_b = (analysis.mean_b.max(1e-6) / analysis.mean_g.max(1e-6)).ln();
        observed_r.push(log_r.clamp(-std::f64::consts::LN_2 * 2.0, std::f64::consts::LN_2 * 2.0));
        observed_b.push(log_b.clamp(-std::f64::consts::LN_2 * 2.0, std::f64::consts::LN_2 * 2.0));
    }

    let smooth_r = smooth_series(&observed_r, 2, 6);
    let smooth_b = smooth_series(&observed_b, 2, 6);
    let anchor_r = median(&smooth_r);
    let anchor_b = median(&smooth_b);
    let temperature = white_balance
        .temperature_factors()
        .map(|value| value.ln() as f64);
    let temperature_r = temperature[0];
    let temperature_b = temperature[2];

    observed_r
        .iter()
        .zip(observed_b.iter())
        .zip(smooth_r.iter().zip(smooth_b.iter()))
        .map(|((observed_r, observed_b), (smooth_r, smooth_b))| {
            // 30% of the sequence median reduces slow target drift, while 70%
            // local smoothing follows gradual lighting changes.
            let target_r = 0.7 * smooth_r + 0.3 * anchor_r + temperature_r;
            let target_b = 0.7 * smooth_b + 0.3 * anchor_b + temperature_b;
            let mut gains = [
                (target_r - observed_r).exp().clamp(0.2, 5.0) as f32,
                1.0,
                (target_b - observed_b).exp().clamp(0.2, 5.0) as f32,
            ];
            // Keep neutral luminance stable; exposure normalization is handled
            // separately by exposure_gains().
            let luminance = 0.2126 * gains[0] + 0.7152 * gains[1] + 0.0722 * gains[2];
            if luminance > 1e-6 {
                for gain in &mut gains {
                    *gain = (*gain / luminance).clamp(0.15, 6.0);
                }
            }
            gains
        })
        .collect()
}

/// Computes cumulative translation, smooths the intentional camera path, and
/// derives a common crop rectangle that removes the residual shake.
fn stabilization_crops(
    images: &[InputImage],
    caches: &[FrameCache],
) -> Result<(Vec<Option<CropRect>>, u32, u32, bool)> {
    let first = &caches[0].analysis;
    let width = first.width;
    let height = first.height;
    if width == 0 || height == 0 {
        return Err(StabilizationError("first frame has zero dimensions".to_string()).into());
    }

    // A single frame has no shake to remove and no alignment reference.
    if caches.len() == 1 {
        return Ok((vec![None], width, height, false));
    }

    for (image, cache) in images.iter().zip(caches.iter()) {
        if cache.analysis.width != width || cache.analysis.height != height {
            return Err(StabilizationError(format!(
                "frame '{}' is {}x{} but the first frame is {}x{}; \
                 crop stabilization requires identical dimensions",
                image.file_name, cache.analysis.width, cache.analysis.height, width, height
            ))
            .into());
        }
        if cache.analysis.sharpness < MIN_SHARPNESS {
            return Err(StabilizationError(format!(
                "frame '{}' is too blurred to stabilize \
                 (sharpness {:.5} < minimum {:.5}); use -s false to process it without stabilization",
                image.file_name, cache.analysis.sharpness, MIN_SHARPNESS
            ))
            .into());
        }
    }

    let mut path_x = Vec::with_capacity(caches.len());
    let mut path_y = Vec::with_capacity(caches.len());
    let mut x = 0.0_f64;
    let mut y = 0.0_f64;
    path_x.push(0.0);
    path_y.push(0.0);
    for (index, cache) in caches.iter().enumerate().skip(1) {
        if !cache.motion.valid {
            return Err(StabilizationError(format!(
                "frame '{}' could not be aligned to its predecessor \
                 (phase-correlation confidence {:.2}); use -s false to keep it uncropped",
                images[index].file_name, cache.motion.confidence
            ))
            .into());
        }
        x += cache.motion.dx;
        y += cache.motion.dy;
        path_x.push(x);
        path_y.push(y);
    }

    let smooth_x = smooth_series(&path_x, 4, 12);
    let smooth_y = smooth_series(&path_y, 4, 12);
    let residual_x: Vec<f64> = path_x
        .iter()
        .zip(smooth_x.iter())
        .map(|(path, smooth)| path - smooth)
        .collect();
    let residual_y: Vec<f64> = path_y
        .iter()
        .zip(smooth_y.iter())
        .map(|(path, smooth)| path - smooth)
        .collect();

    let min_x = residual_x.iter().copied().fold(f64::INFINITY, f64::min);
    let max_x = residual_x.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let min_y = residual_y.iter().copied().fold(f64::INFINITY, f64::min);
    let max_y = residual_y.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let range_x = (max_x - min_x).max(0.0);
    let range_y = (max_y - min_y).max(0.0);

    if !min_x.is_finite() || !min_y.is_finite() {
        return Err(StabilizationError("could not compute a stable crop path".to_string()).into());
    }

    let pad_x = if range_x > 0.0 { 2_u32 } else { 0_u32 };
    let pad_y = if range_y > 0.0 { 2_u32 } else { 0_u32 };
    let crop_width = width.saturating_sub(range_x.ceil() as u32 + 2 * pad_x);
    let crop_height = height.saturating_sub(range_y.ceil() as u32 + 2 * pad_y);
    if crop_width < (width / 4).max(1) || crop_height < (height / 4).max(1) {
        return Err(StabilizationError(format!(
            "required crop would leave only {}x{} pixels from {}x{}; motion is too large \
             for crop-based stabilization",
            crop_width, crop_height, width, height
        ))
        .into());
    }

    let cropped = crop_width != width || crop_height != height;
    if !cropped {
        return Ok((vec![None; caches.len()], width, height, false));
    }

    let mut crops = Vec::with_capacity(caches.len());
    for (x_offset, y_offset) in residual_x.iter().zip(residual_y.iter()) {
        let crop_x = ((x_offset - min_x).round() as i64 + pad_x as i64)
            .clamp(0, (width - crop_width) as i64) as u32;
        let crop_y = ((y_offset - min_y).round() as i64 + pad_y as i64)
            .clamp(0, (height - crop_height) as i64) as u32;
        crops.push(Some(CropRect {
            x: crop_x,
            y: crop_y,
            width: crop_width,
            height: crop_height,
        }));
    }

    Ok((crops, crop_width, crop_height, true))
}

fn srgb_to_linear_f64(value: f64) -> f64 {
    let encoded = value.clamp(0.0, 1.0);
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// Median filter followed by a moving average. Boundaries use reflection so
/// the first and last frames keep the sequence's overall trend.
fn smooth_series(values: &[f64], median_radius: usize, average_radius: usize) -> Vec<f64> {
    if values.is_empty() {
        return Vec::new();
    }
    let filtered = median_filter(values, median_radius);
    moving_average(&filtered, average_radius)
}

fn median_filter(values: &[f64], radius: usize) -> Vec<f64> {
    if radius == 0 || values.len() == 1 {
        return values.to_vec();
    }
    let mut output = Vec::with_capacity(values.len());
    let mut window = Vec::new();
    for index in 0..values.len() {
        window.clear();
        for offset in -(radius as isize)..=(radius as isize) {
            let source = reflect_index(index as isize + offset, values.len());
            window.push(values[source]);
        }
        window.sort_by(|a, b| a.total_cmp(b));
        output.push(window[window.len() / 2]);
    }
    output
}

fn moving_average(values: &[f64], radius: usize) -> Vec<f64> {
    if radius == 0 || values.len() == 1 {
        return values.to_vec();
    }
    let mut output = Vec::with_capacity(values.len());
    for index in 0..values.len() {
        let mut sum = 0.0;
        let mut count = 0.0;
        for offset in -(radius as isize)..=(radius as isize) {
            let source = reflect_index(index as isize + offset, values.len());
            sum += values[source];
            count += 1.0;
        }
        output.push(sum / count);
    }
    output
}

fn reflect_index(index: isize, length: usize) -> usize {
    if length <= 1 {
        return 0;
    }
    let period = (2 * length - 2) as isize;
    let wrapped = index.rem_euclid(period);
    if wrapped < length as isize {
        wrapped as usize
    } else {
        (period - wrapped) as usize
    }
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    sorted[sorted.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_average_preserves_constant_series() {
        let values = vec![4.0; 32];
        let smoothed = smooth_series(&values, 2, 8);
        assert!(smoothed.iter().all(|value| (*value - 4.0).abs() < 1e-9));
    }

    #[test]
    fn smoothing_reduces_an_impulse() {
        let mut values = vec![0.0; 32];
        values[16] = 1.0;
        let smoothed = smooth_series(&values, 2, 4);
        assert!(smoothed[16] < 0.25);
    }

    #[test]
    fn reflection_handles_edges() {
        assert_eq!(reflect_index(-1, 5), 1);
        assert_eq!(reflect_index(5, 5), 3);
        assert_eq!(reflect_index(6, 5), 2);
    }
}
