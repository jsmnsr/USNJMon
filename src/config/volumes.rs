//! Validate local volume selectors and match discovered identities and aliases.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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
