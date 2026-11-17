//! Decoding of Sony ARW raws and ordinary JPEG/PNG images.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use image::metadata::Orientation as ImageOrientation;
use image::{DynamicImage, ImageDecoder, ImageReader, RgbImage};
use rawler::imgop::develop::RawDevelop;

/// A decoded, EXIF-oriented RGB image plus metadata obtained during decoding.
pub struct DecodedImage {
    pub image: RgbImage,
    /// RAW make, used as a fallback when the EXIF package cannot read an ARW.
    pub raw_make: Option<String>,
    /// RAW model, used as a fallback when the EXIF package cannot read an ARW.
    pub raw_model: Option<String>,
}

impl DecodedImage {
    /// Returns the oriented dimensions.
    pub fn dimensions(&self) -> (u32, u32) {
        self.image.dimensions()
    }
}

/// Decodes one supported file. ARW files go through the pure-Rust `rawler`
/// decoder; JPEG and PNG files use the `image` crate.
pub fn decode(path: &Path) -> Result<DecodedImage> {
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "arw" => decode_arw(path),
        "jpg" | "jpeg" | "png" => decode_standard(path),
        other => bail!(
            "unsupported input format '.{other}' for '{}'; expected ARW, JPG or PNG",
            path.display()
        ),
    }
}

/// Decodes a JPEG or PNG and applies the orientation advertised by its decoder.
fn decode_standard(path: &Path) -> Result<DecodedImage> {
    let reader = ImageReader::open(path)
        .with_context(|| format!("cannot open image '{}'", path.display()))?
        .with_guessed_format()
        .with_context(|| format!("cannot detect image format of '{}'", path.display()))?;

    let mut decoder = reader
        .into_decoder()
        .with_context(|| format!("cannot initialize decoder for '{}'", path.display()))?;
    let orientation = decoder
        .orientation()
        .unwrap_or(ImageOrientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder)
        .with_context(|| format!("cannot decode image '{}'", path.display()))?;
    image.apply_orientation(orientation);
    let rgb = image.to_rgb8();

    Ok(DecodedImage {
        image: rgb,
        raw_make: None,
        raw_model: None,
    })
}

/// Decodes a Sony ARW through rawler's demosaic, camera-WB and sRGB pipeline.
fn decode_arw(path: &Path) -> Result<DecodedImage> {
    let decoded = catch_unwind(AssertUnwindSafe(|| rawler::decode_file(path)))
        .map_err(|payload| {
            anyhow!(
                "RAW decoder panicked for '{}': {}",
                path.display(),
                panic_text(&payload)
            )
        })?
        .with_context(|| format!("cannot decode Sony ARW '{}'", path.display()))?;

    let raw_make =
        non_empty(decoded.clean_make.clone()).or_else(|| non_empty(decoded.make.clone()));
    let raw_model =
        non_empty(decoded.clean_model.clone()).or_else(|| non_empty(decoded.model.clone()));
    let orientation = ImageOrientation::from_exif(decoded.orientation.to_u16() as u8)
        .unwrap_or(ImageOrientation::NoTransforms);

    // RawDevelop performs black/white-level scaling, Bayer demosaic, camera
    // white balance, the camera-to-sRGB color matrix, and sRGB gamma.
    let intermediate = RawDevelop::default()
        .develop_intermediate(&decoded)
        .with_context(|| format!("cannot develop Sony ARW '{}'", path.display()))?;
    let mut image = intermediate.to_dynamic_image().ok_or_else(|| {
        anyhow!(
            "RAW pipeline returned an empty image for '{}'",
            path.display()
        )
    })?;
    image.apply_orientation(orientation);
    let rgb = image.to_rgb8();
    if rgb.width() == 0 || rgb.height() == 0 {
        bail!("decoded ARW '{}' has zero dimensions", path.display());
    }

    Ok(DecodedImage {
        image: rgb,
        raw_make,
        raw_model,
    })
}

/// Normalizes an optional metadata string.
fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Renders a panic payload into a human-readable message.
fn panic_text(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}
