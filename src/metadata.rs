//! EXIF metadata extraction using the pure-Rust `kamadak-exif` crate.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use anyhow::{Context, Result};
use exif::{In, Tag};
use serde::{Deserialize, Serialize};

/// The subset of EXIF metadata that is useful for a timelapse record and cache.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExifSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub make: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lens_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_time_original: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposure_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub f_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iso: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focal_length: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub white_balance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_space: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposure_bias: Option<String>,
    /// Orientation tag value, 1 when absent or invalid.
    #[serde(default = "default_orientation")]
    pub orientation: u16,
    /// Populated when a metadata container exists but could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_error: Option<String>,
}

fn default_orientation() -> u16 {
    1
}

impl Default for ExifSummary {
    fn default() -> Self {
        Self {
            make: None,
            model: None,
            lens_model: None,
            date_time_original: None,
            exposure_time: None,
            f_number: None,
            iso: None,
            focal_length: None,
            white_balance: None,
            color_space: None,
            exposure_bias: None,
            orientation: 1,
            read_error: None,
        }
    }
}

impl ExifSummary {
    /// Fills missing camera identification from the RAW decoder's own metadata.
    pub fn fill_camera(&mut self, make: Option<&str>, model: Option<&str>) {
        if self.make.is_none() {
            self.make = make.map(str::to_owned);
        }
        if self.model.is_none() {
            self.model = model.map(str::to_owned);
        }
    }
}

/// Reads EXIF metadata from a TIFF-based RAW, JPEG or PNG file.
///
/// Absence of EXIF is normal (especially for PNG), so it is not treated as an
/// error. A genuinely malformed metadata block is recorded in `read_error` so
/// the caller can emit a warning without stopping the timelapse.
pub fn read_exif(path: &Path) -> Result<ExifSummary> {
    let file = File::open(path)
        .with_context(|| format!("cannot open '{}' for metadata", path.display()))?;
    let mut reader = BufReader::new(file);
    let parsed = exif::Reader::new().read_from_container(&mut reader);

    let exif = match parsed {
        Ok(exif) => exif,
        Err(exif::Error::NotFound(_)) => return Ok(ExifSummary::default()),
        Err(error) => {
            return Ok(ExifSummary {
                read_error: Some(error.to_string()),
                ..ExifSummary::default()
            });
        }
    };

    let mut summary = ExifSummary {
        make: text_field(&exif, Tag::Make),
        model: text_field(&exif, Tag::Model),
        lens_model: text_field(&exif, Tag::LensModel),
        date_time_original: text_field(&exif, Tag::DateTimeOriginal),
        exposure_time: text_field(&exif, Tag::ExposureTime),
        f_number: text_field(&exif, Tag::FNumber),
        iso: exif
            .get_field(Tag::PhotographicSensitivity, In::PRIMARY)
            .and_then(|field| field.value.get_uint(0)),
        focal_length: text_field(&exif, Tag::FocalLength),
        white_balance: text_field(&exif, Tag::WhiteBalance),
        color_space: text_field(&exif, Tag::ColorSpace),
        exposure_bias: text_field(&exif, Tag::ExposureBiasValue),
        orientation: exif
            .get_field(Tag::Orientation, In::PRIMARY)
            .and_then(|field| field.value.get_uint(0))
            .filter(|value| (1..=8).contains(value))
            .map(|value| value as u16)
            .unwrap_or(1),
        read_error: None,
    };

    // Some cameras write a rational ZeroDenominator for absent fields. Treat
    // empty display strings as missing instead of storing noise.
    clear_blank(&mut summary);
    Ok(summary)
}

/// Reads a field and renders it with its standard unit.
fn text_field(exif: &exif::Exif, tag: Tag) -> Option<String> {
    exif.get_field(tag, In::PRIMARY).map(|field| {
        field
            .display_value()
            .with_unit(exif)
            .to_string()
            .trim_matches('"')
            .trim()
            .to_string()
    })
}

/// Removes empty optional strings produced by blank EXIF values.
fn clear_blank(summary: &mut ExifSummary) {
    for value in [
        &mut summary.make,
        &mut summary.model,
        &mut summary.lens_model,
        &mut summary.date_time_original,
        &mut summary.exposure_time,
        &mut summary.f_number,
        &mut summary.focal_length,
        &mut summary.white_balance,
        &mut summary.color_space,
        &mut summary.exposure_bias,
    ] {
        if value.as_deref().is_some_and(|text| text.is_empty()) {
            *value = None;
        }
    }
}
