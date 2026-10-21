use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, io::Read, path::Path};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub volumes: Vec<VolumeConfig>,
    pub poll_interval_ms: u64,
    pub event_interval_ms: u64,
    pub retry_interval_ms: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeConfig {
    pub name: String,
    pub path: String,
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
        ensure!(
            !self.volumes.is_empty() && self.volumes.len() <= 26,
            "configure 1..26 volumes"
        );
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
        for volume in &self.volumes {
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
                paths.insert(volume.path.to_ascii_lowercase()),
                "duplicate volume path"
            );
            ensure!(
                volume.path.len() <= 1024
                    && volume.path.ends_with('\\')
                    && !volume.path.contains('\0')
                    && (volume.path.starts_with(r"\\?\Volume{")
                        || (volume.path.len() >= 3
                            && volume.path.as_bytes()[0].is_ascii_alphabetic()
                            && volume.path.as_bytes()[1] == b':'
                            && volume.path.as_bytes()[2] == b'\\')),
                "volume path must be a local mount-point root or volume GUID path, with trailing backslash"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        serde_json::from_str(include_str!("..\\config.example.json")).unwrap()
    }

    #[test]
    fn valid_local_paths_and_rejected_unsafe_state_names() {
        for path in [
            r"C:\",
            r"C:\mounts\data\",
            r"\\?\Volume{00000000-0000-0000-0000-000000000001}\",
        ] {
            let mut c = config();
            c.volumes[0].path = path.into();
            c.validate().unwrap();
        }
        for name in ["", "../escape", "..", "con", "NUL", "COM1", "lpt9", "a:b"] {
            let mut c = config();
            c.volumes[0].name = name.into();
            assert!(c.validate().is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_remote_paths_duplicate_names_and_unbounded_rates() {
        for path in [r"\\server\share\", r"C:", r"relative\", "C:\\\0"] {
            let mut c = config();
            c.volumes[0].path = path.into();
            assert!(c.validate().is_err(), "{path}");
        }
        let mut c = config();
        c.volumes.push(VolumeConfig {
            name: "SYSTEM".into(),
            path: r"D:\".into(),
        });
        assert!(c.validate().is_err());
        let mut c = config();
        c.event_interval_ms = 0;
        assert!(c.validate().is_err());
        assert!(serde_json::from_str::<Config>(r#"{"extra":true}"#).is_err());
    }
}
