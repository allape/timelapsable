//! Persistent per-frame analysis cache with completeness checks and atomic writes.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::analysis::{ImageAnalysis, MotionEstimate};
use crate::atomic::atomic_write;
use crate::metadata::ExifSummary;
use crate::source::SourceId;

/// Bump when the meaning or required set of cached fields changes.
pub const CACHE_VERSION: u32 = 1;

/// Everything quantified for one frame, stored as `.meta.<stem>.toml`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrameCache {
    pub cache_version: u32,
    pub complete: bool,
    pub checksum: String,
    pub source: SourceId,
    /// Identity of the previous frame at the time motion was measured.
    pub prev: Option<SourceId>,
    pub analysis: ImageAnalysis,
    pub motion: MotionEstimate,
    pub exif: ExifSummary,
}

/// Result of attempting to reuse a cache entry.
pub enum CacheLoad {
    Hit(Box<FrameCache>),
    Miss { reason: String },
}

impl FrameCache {
    /// Builds a cache entry with the checksum still unset.
    pub fn new(
        source: SourceId,
        prev: Option<SourceId>,
        analysis: ImageAnalysis,
        motion: MotionEstimate,
        exif: ExifSummary,
    ) -> Self {
        Self {
            cache_version: CACHE_VERSION,
            complete: true,
            checksum: String::new(),
            source,
            prev,
            analysis,
            motion,
            exif,
        }
    }

    /// Loads a cache only when it is complete, structurally valid, checksummed,
    /// and still describes the expected source and predecessor.
    pub fn load(
        path: &Path,
        expected_source: &SourceId,
        expected_prev: Option<&SourceId>,
    ) -> CacheLoad {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return CacheLoad::Miss {
                    reason: "no cache file".to_string(),
                };
            }
            Err(error) => {
                return CacheLoad::Miss {
                    reason: format!("cannot read cache: {error}"),
                };
            }
        };
        let cache: FrameCache = match toml::from_str(&text) {
            Ok(cache) => cache,
            Err(error) => {
                return CacheLoad::Miss {
                    reason: format!("invalid cache TOML: {error}"),
                };
            }
        };

        if cache.cache_version != CACHE_VERSION {
            return CacheLoad::Miss {
                reason: format!(
                    "cache version {} does not match {}",
                    cache.cache_version, CACHE_VERSION
                ),
            };
        }
        if !cache.complete {
            return CacheLoad::Miss {
                reason: "cache is marked incomplete".to_string(),
            };
        }
        if cache.checksum.is_empty() {
            return CacheLoad::Miss {
                reason: "cache checksum is missing".to_string(),
            };
        }
        if !cache.checksum_matches() {
            return CacheLoad::Miss {
                reason: "cache checksum does not match its contents".to_string(),
            };
        }
        if &cache.source != expected_source {
            return CacheLoad::Miss {
                reason: "source file changed since analysis".to_string(),
            };
        }
        if cache.prev.as_ref() != expected_prev {
            return CacheLoad::Miss {
                reason: "previous frame changed since analysis".to_string(),
            };
        }
        if !cache.analysis.is_sane() || !cache.motion.is_sane() {
            return CacheLoad::Miss {
                reason: "cache contains invalid measurements".to_string(),
            };
        }
        if expected_prev.is_none() && (cache.motion.dx != 0.0 || cache.motion.dy != 0.0) {
            return CacheLoad::Miss {
                reason: "first frame has non-zero cached motion".to_string(),
            };
        }

        CacheLoad::Hit(Box::new(cache))
    }

    /// Persists this entry atomically after filling in its checksum.
    pub fn write(&mut self, path: &Path) -> Result<()> {
        self.cache_version = CACHE_VERSION;
        self.complete = true;
        self.checksum.clear();
        self.checksum = self.compute_checksum()?;
        let text = toml::to_string_pretty(self).context("cannot serialize analysis cache")?;
        atomic_write(path, |file| {
            use std::io::Write;
            file.write_all(text.as_bytes())
        })
        .with_context(|| format!("cannot write analysis cache '{}'", path.display()))
    }

    /// Hashes the TOML payload with the checksum field blanked.
    fn compute_checksum(&self) -> Result<String> {
        let mut payload = self.clone();
        payload.checksum.clear();
        let text = toml::to_string(&payload).context("cannot serialize cache for checksum")?;
        let digest = Sha256::digest(text.as_bytes());
        Ok(format!("{digest:x}"))
    }

    fn checksum_matches(&self) -> bool {
        match self.compute_checksum() {
            Ok(expected) => expected == self.checksum,
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cache() -> FrameCache {
        FrameCache::new(
            SourceId {
                file: "A.JPG".into(),
                size: 10,
                mtime_ns: 20,
            },
            None,
            ImageAnalysis {
                width: 100,
                height: 80,
                mean_r: 0.2,
                mean_g: 0.25,
                mean_b: 0.3,
                median_luma: 0.4,
                p05_luma: 0.1,
                p95_luma: 0.8,
                highlight_fraction: 0.01,
                sharpness: 0.02,
            },
            MotionEstimate {
                dx: 0.0,
                dy: 0.0,
                confidence: 1_000_000.0,
                valid: true,
            },
            ExifSummary::default(),
        )
    }

    #[test]
    fn checksum_detects_tampering() {
        let mut cache = sample_cache();
        cache.checksum = cache.compute_checksum().unwrap();
        assert!(cache.checksum_matches());
        cache.analysis.median_luma = 0.5;
        assert!(!cache.checksum_matches());
    }
}
