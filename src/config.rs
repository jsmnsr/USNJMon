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
    pub path_filters: crate::filter::PathFilters,
    pub poll_interval_ms: u64,
    pub event_interval_ms: u64,
    pub retry_interval_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Volumes {
    Explicit(Vec<VolumeConfig>),
    Discover(VolumeSelectors),
}

impl Default for Volumes {
    fn default() -> Self {
        Self::Discover(VolumeSelectors::default())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct VolumeSelectors {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct DiscoveredVolume {
    pub path: String,
    pub mounts: Vec<String>,
}

pub fn volume_key(path: &str) -> Result<String> {
    let path = path.replace('/', "\\");
    ensure!(
        !path.contains('\0') && path.len() <= 1024,
        "invalid volume selector"
    );
    let root = path.trim_end_matches('\\');
    let lower = root.to_ascii_lowercase();
    let valid_guid = lower
        .strip_prefix(r"\\?\volume{")
        .and_then(|value| value.strip_suffix('}'))
        .is_some_and(|id| {
            id.len() == 36
                && id.bytes().enumerate().all(|(i, b)| {
                    if [8, 13, 18, 23].contains(&i) {
                        b == b'-'
                    } else {
                        b.is_ascii_hexdigit()
                    }
                })
        });
    let valid_mount = root.len() >= 2
        && root.as_bytes()[0].is_ascii_alphabetic()
        && root.as_bytes()[1] == b':'
        && (root.len() == 2 || root.as_bytes()[2] == b'\\')
        && root[2..]
            .split('\\')
            .skip(1)
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && !root[2..]
            .chars()
            .any(|c| c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'));
    ensure!(
        valid_guid || valid_mount,
        "volume selector must be an exact local drive, mount-point root or volume GUID"
    );
    Ok(format!("{lower}\\"))
}

impl VolumeSelectors {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.include.len() + self.exclude.len() <= 256,
            "at most 256 volume selectors"
        );
        for path in self.include.iter().chain(&self.exclude) {
            volume_key(path)?;
        }
        Ok(())
    }

    pub fn select(&self, discovered: Vec<DiscoveredVolume>) -> Result<Vec<VolumeConfig>> {
        self.validate()?;
        let includes = self
            .include
            .iter()
            .map(|p| volume_key(p))
            .collect::<Result<Vec<_>>>()?;
        let excludes = self
            .exclude
            .iter()
            .map(|p| volume_key(p))
            .collect::<Result<Vec<_>>>()?;
        let mut matched = HashSet::new();
        let mut seen = HashSet::new();
        let mut selected = Vec::new();
        for volume in discovered {
            let key = volume_key(&volume.path)?;
            let mut aliases = vec![key.clone()];
            aliases.extend(
                volume
                    .mounts
                    .iter()
                    .map(|p| volume_key(p))
                    .collect::<Result<Vec<_>>>()?,
            );
            for include in &includes {
                if aliases.contains(include) {
                    matched.insert(include.clone());
                }
            }
            if (includes.is_empty() || includes.iter().any(|p| aliases.contains(p)))
                && !excludes.iter().any(|p| aliases.contains(p))
                && seen.insert(key.clone())
            {
                let id = key
                    .strip_prefix(r"\\?\volume{")
                    .and_then(|v| v.strip_suffix("}\\"))
                    .context("discovered volume lacks a GUID identity")?;
                selected.push(VolumeConfig {
                    name: format!("volume-{id}"),
                    path: key,
                });
            }
        }
        for include in &includes {
            ensure!(
                matched.contains(include),
                "included volume {include} was not discovered as accessible NTFS"
            );
        }
        ensure!(
            !selected.is_empty(),
            "volume selectors matched no NTFS volumes"
        );
        ensure!(
            selected.len() <= 128,
            "more than 128 volumes selected; narrow the volume selectors"
        );
        selected.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(selected)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
mod tests {
    use super::*;

    fn config() -> Config {
        serde_json::from_str(
            r#"{
            "volumes":[{"name":"system","path":"C:\\"}],
            "poll_interval_ms":200,"event_interval_ms":2,"retry_interval_ms":10000
        }"#,
        )
        .unwrap()
    }

    fn explicit(config: &mut Config) -> &mut Vec<VolumeConfig> {
        let Volumes::Explicit(volumes) = &mut config.volumes else {
            panic!("expected explicit")
        };
        volumes
    }

    #[test]
    fn valid_local_paths_and_rejected_unsafe_state_names() {
        for path in [
            r"C:\",
            r"C:\mounts\data\",
            r"\\?\Volume{00000000-0000-0000-0000-000000000001}\",
        ] {
            let mut c = config();
            explicit(&mut c)[0].path = path.into();
            c.validate().unwrap();
        }
        for name in ["", "../escape", "..", "con", "NUL", "COM1", "lpt9", "a:b"] {
            let mut c = config();
            explicit(&mut c)[0].name = name.into();
            assert!(c.validate().is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_remote_paths_duplicate_names_and_unbounded_rates() {
        for path in [r"\\server\share\", r"C:", r"relative\", "C:\\\0"] {
            let mut c = config();
            explicit(&mut c)[0].path = path.into();
            assert!(c.validate().is_err(), "{path}");
        }
        let mut c = config();
        explicit(&mut c).push(VolumeConfig {
            name: "SYSTEM".into(),
            path: r"D:\".into(),
        });
        assert!(c.validate().is_err());
        let mut c = config();
        c.event_interval_ms = 0;
        assert!(c.validate().is_err());
        assert!(serde_json::from_str::<Config>(r#"{"extra":true}"#).is_err());
    }

    #[test]
    fn default_discovery_selects_all_and_exclusion_wins_across_aliases() {
        let a = DiscoveredVolume {
            path: r"\\?\Volume{00000000-0000-0000-0000-000000000001}\".into(),
            mounts: vec![r"C:\".into(), r"D:\mount\data\".into()],
        };
        let b = DiscoveredVolume {
            path: r"\\?\Volume{00000000-0000-0000-0000-000000000002}\".into(),
            mounts: vec![],
        };
        let all = VolumeSelectors::default()
            .select(vec![a.clone(), b.clone(), a.clone()])
            .unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "volume-00000000-0000-0000-0000-000000000001");
        let selectors = VolumeSelectors {
            include: vec![],
            exclude: vec![r"d:\MOUNT\data".into()],
        };
        assert_eq!(
            selectors.select(vec![a.clone(), b.clone()]).unwrap()[0].path,
            volume_key(&b.path).unwrap()
        );
        let selectors = VolumeSelectors {
            include: vec!["c:".into(), b.path.clone()],
            exclude: vec![a.path.clone()],
        };
        assert_eq!(selectors.select(vec![a, b]).unwrap().len(), 1);
        assert!(
            VolumeSelectors {
                include: vec!["z:".into()],
                exclude: vec![]
            }
            .select(vec![])
            .is_err()
        );
        let config: Config = serde_json::from_str(
            r#"{"poll_interval_ms":200,"event_interval_ms":2,"retry_interval_ms":10000}"#,
        )
        .unwrap();
        assert!(matches!(config.volumes, Volumes::Discover(_)));
        config.validate().unwrap();
        let example: Config =
            serde_json::from_str(include_str!("..\\config.example.json")).unwrap();
        example.validate().unwrap();
        assert!(serde_json::from_str::<Volumes>(r#"{"incldue":["C:\\"]}"#).is_err());
        for bad in [
            r"\\server\share",
            r"C:\..\x",
            r"C:\*",
            r"\\?\Volume{wrong}\",
        ] {
            assert!(volume_key(bad).is_err());
        }
    }
}
