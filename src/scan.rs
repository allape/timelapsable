//! Scanning the input folder and deriving deterministic output names.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::cli::OutputFormat;
use crate::source::SourceId;

/// One supported input image.
#[derive(Clone, Debug)]
pub struct InputImage {
    pub path: PathBuf,
    pub file_name: String,
    /// Source name without its final extension.
    pub stem: String,
    pub source: SourceId,
}

impl InputImage {
    /// Encoded output file name, for example `DSC0001.tlpb.jpg`.
    pub fn output_name(&self, format: OutputFormat) -> String {
        format!("{}.tlpb{}", self.stem, format.extension())
    }

    /// Analysis cache file name, for example `.meta.DSC0001.toml`.
    pub fn meta_name(&self) -> String {
        format!(".meta.{}.toml", self.stem)
    }
}

/// Returns true for the extensions this tool can decode.
pub fn is_supported_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "arw" | "jpg" | "jpeg" | "png"
    )
}

/// Scans exactly one directory level and returns supported images in
/// case-insensitive alphabetical file-name order.
pub fn scan_folder(folder: &Path) -> Result<Vec<InputImage>> {
    if !folder.exists() {
        bail!("input folder '{}' does not exist", folder.display());
    }
    if !folder.is_dir() {
        bail!("input path '{}' is not a directory", folder.display());
    }

    let mut images = Vec::new();
    let entries = fs::read_dir(folder)
        .with_context(|| format!("cannot read input folder '{}'", folder.display()))?;
    for entry in entries {
        let entry = entry
            .with_context(|| format!("failed while reading input folder '{}'", folder.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("cannot inspect '{}'", path.display()))?;
        if !file_type.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name.ends_with(".tlpb.jpg")
            || file_name.ends_with(".tlpb.jpeg")
            || file_name.ends_with(".tlpb.png")
        {
            continue;
        }
        let extension = path
            .extension()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !is_supported_extension(&extension) {
            continue;
        }
        let stem = path
            .file_stem()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| file_name.clone());
        let source = SourceId::from_path(&path)?;
        images.push(InputImage {
            path,
            file_name,
            stem,
            source,
        });
    }

    images.sort_by(|left, right| {
        left.file_name
            .to_lowercase()
            .cmp(&right.file_name.to_lowercase())
            .then_with(|| left.file_name.cmp(&right.file_name))
    });

    validate_names(&images)?;
    Ok(images)
}

/// Rejects sequences whose output or metadata names would overwrite each other.
fn validate_names(images: &[InputImage]) -> Result<()> {
    let mut outputs: HashMap<String, &InputImage> = HashMap::new();
    let mut metas: HashMap<String, &InputImage> = HashMap::new();
    for image in images {
        let output = image.output_name(OutputFormat::Jpeg);
        let output_png = image.output_name(OutputFormat::Png);
        for name in [output, output_png] {
            if let Some(previous) = outputs.insert(name.clone(), image) {
                if previous.file_name != image.file_name {
                    bail!(
                        "input files '{}' and '{}' map to the same output name '{}'",
                        previous.file_name,
                        image.file_name,
                        name
                    );
                }
            }
        }
        let meta = image.meta_name();
        if let Some(previous) = metas.insert(meta.clone(), image) {
            if previous.file_name != image.file_name {
                bail!(
                    "input files '{}' and '{}' map to the same metadata cache '{}'",
                    previous.file_name,
                    image.file_name,
                    meta
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_supported_extensions_case_insensitively() {
        assert!(is_supported_extension("ARW"));
        assert!(is_supported_extension("jpg"));
        assert!(is_supported_extension("Jpeg"));
        assert!(is_supported_extension("png"));
        assert!(!is_supported_extension("tif"));
    }

    #[test]
    fn output_names_use_source_stem() {
        let image = InputImage {
            path: PathBuf::from("/tmp/DSC0001.ARW"),
            file_name: "DSC0001.ARW".into(),
            stem: "DSC0001".into(),
            source: SourceId {
                file: "DSC0001.ARW".into(),
                size: 0,
                mtime_ns: 0,
            },
        };
        assert_eq!(image.output_name(OutputFormat::Jpeg), "DSC0001.tlpb.jpg");
        assert_eq!(image.output_name(OutputFormat::Png), "DSC0001.tlpb.png");
        assert_eq!(image.meta_name(), ".meta.DSC0001.toml");
    }
}
