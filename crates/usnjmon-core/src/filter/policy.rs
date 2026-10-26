//! Literal volume-relative path normalization and selection policy.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Unresolved {
    #[default]
    Include,
    Exclude,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PathFilters {
    pub whitelist: Vec<String>,
    pub blacklist: Vec<String>,
    pub unresolved: Unresolved,
}

impl Default for PathFilters {
    fn default() -> Self {
        Self {
            whitelist: Vec::new(),
            blacklist: [
                r"\Windows\Logs",
                r"\Windows\Temp",
                r"\Windows\SoftwareDistribution\Download",
                r"\ProgramData\UsnEventLog",
                r"\ProgramData\USNJMon",
            ]
            .map(String::from)
            .to_vec(),
            unresolved: Unresolved::Include,
        }
    }
}

pub fn normalize_path(path: &str) -> Result<String> {
    let path = path.replace('/', "\\");
    ensure!(
        path.starts_with('\\')
            && !path.starts_with("\\\\")
            && !path
                .chars()
                .any(|c| c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')),
        "path filters require a volume-relative literal path beginning with one backslash (no glob, drive or UNC prefix)"
    );
    let path = path.trim_end_matches('\\');
    if path.is_empty() {
        return Ok("\\".into());
    }
    ensure!(
        path[1..]
            .split('\\')
            .all(|part| !part.is_empty() && part != "." && part != ".."),
        "empty or relative path components are not allowed"
    );
    Ok(path.to_lowercase())
}

#[derive(Clone)]
pub struct Policy {
    whitelist: Vec<String>,
    blacklist: Vec<String>,
    unresolved: Unresolved,
}

impl PathFilters {
    pub fn compile(&self) -> Result<Policy> {
        ensure!(
            self.whitelist.len() + self.blacklist.len() <= 256,
            "at most 256 path rules"
        );
        let normalize = |rules: &[String]| -> Result<Vec<String>> {
            rules
                .iter()
                .map(|path| {
                    ensure!(path.len() <= 4096, "path rule exceeds 4096 bytes");
                    normalize_path(path)
                })
                .collect()
        };
        Ok(Policy {
            whitelist: normalize(&self.whitelist)?,
            blacklist: normalize(&self.blacklist)?,
            unresolved: self.unresolved,
        })
    }
}

fn matches(path: &str, prefix: &str) -> bool {
    prefix == "\\"
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('\\'))
}

#[derive(Debug, PartialEq)]
pub enum Decision {
    Include,
    Blacklisted,
    NotWhitelisted,
    Unresolved,
}

impl Policy {
    pub fn decide(&self, path: Option<&str>) -> Result<Decision> {
        let Some(path) = path else {
            return Ok(if self.unresolved == Unresolved::Include {
                Decision::Include
            } else {
                Decision::Unresolved
            });
        };
        let path = normalize_path(path)?;
        Ok(if self.blacklist.iter().any(|rule| matches(&path, rule)) {
            Decision::Blacklisted
        } else if !self.whitelist.is_empty()
            && !self.whitelist.iter().any(|rule| matches(&path, rule))
        {
            Decision::NotWhitelisted
        } else {
            Decision::Include
        })
    }
}
