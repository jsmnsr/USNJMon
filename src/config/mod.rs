//! Bounded application configuration loading and validation.

mod volumes;
pub use volumes::{DiscoveredVolume, VolumeConfig, VolumeSelectors, Volumes, volume_key};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub volumes: Volumes,
    #[serde(default)]
    pub path_filters: usnjmon_core::filter::PathFilters,
    pub poll_interval_ms: u64,
    pub event_interval_ms: u64,
    pub retry_interval_ms: u64,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .with_context(|| format!("read {}", path.display()))?
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 64 * 1024, "configuration exceeds 64 KiB");
        let config: Self = serde_json::from_slice(&bytes).context("parse configuration")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        self.path_filters.compile()?;
        let volumes = match &self.volumes {
            Volumes::Explicit(volumes) => {
                ensure!(
                    !volumes.is_empty() && volumes.len() <= 128,
                    "configure 1..128 volumes"
                );
                volumes.as_slice()
            }
            Volumes::Discover(selectors) => {
                selectors.validate()?;
                &[]
            }
        };
        ensure!(
            (100..=60_000).contains(&self.poll_interval_ms),
            "poll_interval_ms must be 100..60000"
        );
        ensure!(
            (1..=1_000).contains(&self.event_interval_ms),
            "event_interval_ms must be 1..1000"
        );
        ensure!(
            (1_000..=300_000).contains(&self.retry_interval_ms),
            "retry_interval_ms must be 1000..300000"
        );
        let mut names = HashSet::new();
        let mut paths = HashSet::new();
        for volume in volumes {
            ensure!(
                !volume.name.is_empty()
                    && volume.name.len() <= 48
                    && volume
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                "volume names must be 1..48 ASCII letters, digits or hyphens"
            );
            ensure!(
                names.insert(volume.name.to_ascii_lowercase()),
                "duplicate volume name"
            );
            let upper = volume.name.to_ascii_uppercase();
            ensure!(
                !matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                    && !(upper.len() == 4
                        && (upper.starts_with("COM") || upper.starts_with("LPT"))
                        && matches!(upper.as_bytes()[3], b'1'..=b'9')),
                "volume name is a reserved Windows device name"
            );
            ensure!(
                paths.insert(volume_key(&volume.path)?),
                "duplicate volume path"
            );
            ensure!(
                volume.path.ends_with('\\'),
                "volume path must be a local mount-point root or volume GUID path, with trailing backslash"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests;
