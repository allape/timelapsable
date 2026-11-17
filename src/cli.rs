//! Command-line interface definition.

use std::path::PathBuf;
use std::str::FromStr;

use clap::{ArgAction, Parser};

use crate::error::UsageError;

/// Supported output encodings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFormat {
    Jpeg,
    Png,
}

impl OutputFormat {
    /// Extension including the leading dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => ".jpg",
            Self::Png => ".png",
        }
    }
}

impl FromStr for OutputFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "jpg" | ".jpg" | "jpeg" | ".jpeg" => Ok(Self::Jpeg),
            "png" | ".png" => Ok(Self::Png),
            other => Err(format!(
                "unsupported output format '{other}'; expected .jpg or .png"
            )),
        }
    }
}

/// Command-line options accepted by `timelapsable`.
#[derive(Debug, Parser)]
#[command(
    name = "timelapsable",
    version,
    about = "Stabilize and color-match an alphabetical image-sequence timelapse",
    after_help = "Image formats: Sony ARW, JPEG and PNG.\n\
                  Outputs are written as <source-stem>.tlpb.jpg|png to <folder>/.timelapsable/\n\
                  unless --output names another directory. Existing images are skipped\n\
                  unless --overwrite is given. Valid analysis caches are reused unless\n\
                  --no-cache regenerates them.\n\
                  Stabilization failure exits with status 4."
)]
pub struct Cli {
    /// Directory containing the image sequence (single level, no recursion).
    #[arg(short = 'f', long = "folder", value_name = "DIR", default_value = ".")]
    pub folder: PathBuf,

    /// Directory for rendered images and analysis caches.
    /// Defaults to <folder>/.timelapsable. Relative paths resolve against the
    /// current working directory and the directory is created when missing.
    #[arg(short = 'o', long = "output", value_name = "DIR")]
    pub output: Option<PathBuf>,

    /// Replace output images that already exist. Without this flag, existing
    /// images are skipped so an interrupted run can be resumed.
    #[arg(long = "overwrite", action = ArgAction::SetTrue)]
    pub overwrite: bool,

    /// Regenerate the per-frame analysis caches. Without this flag, valid
    /// .meta.*.toml files are reused; with it, every frame is re-analyzed and
    /// its cache file is rewritten.
    #[arg(long = "no-cache", action = ArgAction::SetTrue)]
    pub no_cache: bool,

    /// Output format: .jpg or .png.
    #[arg(
        long = "format",
        value_name = "FORMAT",
        default_value = ".jpg",
        value_parser = parse_output_format
    )]
    pub format: OutputFormat,

    /// Enable crop-based stabilization. Use -s false to disable it.
    #[arg(
        short = 's',
        long = "steadicam",
        value_name = "BOOL",
        default_value = "true",
        default_missing_value = "true",
        num_args = 0..=1,
        action = ArgAction::Set,
        value_parser = parse_bool
    )]
    pub steadicam: bool,

    /// Manual white balance in Kelvin (e.g. 5600, 5600K), or a preset such as daylight.
    /// Omit for automatic sequence analysis.
    #[arg(short = 'w', long = "white-balance", value_name = "WB")]
    pub white_balance: Option<String>,

    /// Brightness offset in percent, -100 (black) through 100 (white).
    /// Applied on top of automatic exposure matching.
    #[arg(
        short = 'b',
        long = "brightness",
        value_name = "PERCENT",
        allow_hyphen_values = true,
        value_parser = parse_brightness
    )]
    pub brightness: Option<f64>,
}

/// Parses the output format early so clap can emit a proper usage error.
fn parse_output_format(value: &str) -> Result<OutputFormat, String> {
    value.parse()
}

/// Parses the common textual boolean spellings accepted by `--steadicam`.
fn parse_bool(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => Err(format!("invalid boolean '{other}'; expected true or false")),
    }
}

/// Parses and validates the brightness percentage.
fn parse_brightness(value: &str) -> Result<f64, String> {
    let parsed = value
        .trim()
        .trim_end_matches('%')
        .parse::<f64>()
        .map_err(|_| format!("invalid brightness '{value}'; expected a number in -100..=100"))?;
    if !parsed.is_finite() {
        return Err("brightness must be a finite number".to_string());
    }
    if !(-100.0..=100.0).contains(&parsed) {
        return Err(format!(
            "brightness {parsed} is outside the valid range -100..=100"
        ));
    }
    Ok(parsed)
}

/// Converts CLI values into runtime configuration and emits non-fatal warnings.
pub fn validate(cli: &Cli) -> anyhow::Result<()> {
    if let Some(brightness) = cli.brightness {
        if brightness.abs() > 90.0 {
            eprintln!(
                "warning: brightness {brightness:.1}% is outside the recommended range \
                 [-90, 90]; highlights or shadows may clip heavily"
            );
        }
    }
    if let Some(wb) = &cli.white_balance {
        if wb.trim().is_empty() {
            return Err(UsageError("--white-balance cannot be empty".to_string()).into());
        }
    }
    if let Some(output) = &cli.output {
        if output.as_os_str().is_empty() {
            return Err(UsageError("--output cannot be empty".to_string()).into());
        }
    }
    Ok(())
}
