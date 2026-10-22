use crate::engine::{self, Event, Sink};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;

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

pub struct ResolvedPath {
    pub relative: Option<String>,
    pub full: Option<String>,
    pub status: String,
}

impl ResolvedPath {
    pub fn unresolved(reason: impl std::fmt::Display) -> Self {
        Self {
            relative: None,
            full: None,
            status: format!("unresolved:{reason}"),
        }
    }
}

pub struct FilteringSink<'a, S, R> {
    pub sink: &'a mut S,
    pub policy: &'a Policy,
    pub resolve: R,
    pub volume: &'a str,
    pub lane: &'a str,
    counts: [u64; 4],
}

impl<'a, S, R> FilteringSink<'a, S, R> {
    pub fn new(
        sink: &'a mut S,
        policy: &'a Policy,
        resolve: R,
        volume: &'a str,
        lane: &'a str,
    ) -> Self {
        Self {
            sink,
            policy,
            resolve,
            volume,
            lane,
            counts: [0; 4],
        }
    }
}

impl<S: Sink, R: FnMut(&Event) -> Result<ResolvedPath>> Sink for FilteringSink<'_, S, R> {
    fn write(&mut self, event: &Event) -> Result<()> {
        if !crate::schema::RECORD_IDS.contains(&event.id) {
            return self.sink.write(event);
        }
        let path = (self.resolve)(event)?;
        if path.relative.is_none() {
            self.counts[3] += 1;
        }
        match self.policy.decide(path.relative.as_deref())? {
            Decision::Blacklisted => self.counts[0] += 1,
            Decision::NotWhitelisted => self.counts[1] += 1,
            Decision::Unresolved => self.counts[2] += 1,
            Decision::Include => {
                let mut event = event.clone();
                event.payload["full_path"] = json!(path.full);
                event.payload["path_status"] = json!(path.status);
                self.sink.write(&event)?;
            }
        }
        Ok(())
    }

    fn finish_batch(&mut self) -> Result<()> {
        if self.counts != [0; 4] {
            self.sink.write(&Event::new(
                engine::FILTER_SUMMARY,
                json!({
                    "volume_name": self.volume, "lane": self.lane,
                    "blacklisted": self.counts[0], "not_whitelisted": self.counts[1],
                    "unresolved_excluded": self.counts[2], "unresolved_paths": self.counts[3]
                }),
            ))?;
            self.counts = [0; 4];
        }
        self.sink.finish_batch()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::Journal,
        journal::{fixtures, parse_batch},
        state::{Checkpoint, Store},
        stop::Stop,
    };
    use std::time::Duration;

    #[derive(Default)]
    struct Destination {
        events: Vec<Event>,
        fail_id: Option<u32>,
        saved: Option<Checkpoint>,
    }
    impl Sink for Destination {
        fn write(&mut self, event: &Event) -> Result<()> {
            ensure!(self.fail_id != Some(event.id), "injected write failure");
            self.events.push(event.clone());
            Ok(())
        }
    }
    impl Store for Destination {
        fn save(&mut self, checkpoint: &Checkpoint) -> Result<()> {
            self.saved = Some(checkpoint.clone());
            Ok(())
        }
    }

    #[test]
    fn filtered_batches_advance_only_after_summary_and_accepted_writes() {
        let batch = parse_batch(&fixtures::batch(2, 100, 0x100, &[65]), 100).unwrap();
        let initial = engine::reconcile(
            None,
            "fixture",
            &Journal {
                id: 1,
                first_usn: 100,
                lowest_valid_usn: 0,
                next_usn: 164,
            },
            true,
            false,
        )
        .unwrap()
        .checkpoint;
        let policy = PathFilters::default().compile().unwrap();
        let mut sink = Destination {
            fail_id: Some(engine::FILTER_SUMMARY),
            ..Default::default()
        };
        let mut store = Destination::default();
        let mut checkpoint = initial.clone();
        for failure in [true, false] {
            if !failure {
                sink.fail_id = None;
            }
            let mut filter = FilteringSink::new(
                &mut sink,
                &policy,
                |_: &Event| {
                    Ok(ResolvedPath {
                        relative: Some(r"\Windows\Logs\example.log".into()),
                        full: Some(r"\\?\Volume{fixture}\Windows\Logs\example.log".into()),
                        status: "parent_current_only".into(),
                    })
                },
                "fixture",
                "history",
            );
            let result = engine::deliver(
                &mut checkpoint,
                &batch,
                &mut filter,
                &mut store,
                &Stop::default(),
                Duration::ZERO,
            );
            if failure {
                assert!(result.is_err());
                assert_eq!(checkpoint, initial);
                assert!(store.saved.is_none());
            } else {
                result.unwrap();
                assert_eq!(checkpoint.next_usn, 164);
                assert_eq!(sink.events[0].id, engine::FILTER_SUMMARY);
                assert_eq!(sink.events[0].payload["blacklisted"], 1);
                assert_eq!(sink.events[1].id, engine::CAUGHT_UP);
            }
        }
        assert!(
            !sink
                .events
                .iter()
                .any(|e| crate::schema::RECORD_IDS.contains(&e.id))
        );
    }

    #[test]
    fn unresolved_is_visible_health_bypasses_filters_and_record_failures_propagate() {
        let policy = PathFilters {
            whitelist: vec![r"\only".into()],
            ..Default::default()
        }
        .compile()
        .unwrap();
        let mut sink = Destination::default();
        let mut filter = FilteringSink::new(
            &mut sink,
            &policy,
            |_: &Event| Ok(ResolvedPath::unresolved("parent_open_win32_2")),
            "fixture",
            "live",
        );
        filter
            .write(&Event::new(
                1000,
                json!({"full_path":null,"path_status":"unresolved"}),
            ))
            .unwrap();
        filter
            .write(&Event::new(engine::HEALTH, json!({"error":"unavailable"})))
            .unwrap();
        filter.finish_batch().unwrap();
        assert_eq!(
            sink.events[0].payload["path_status"],
            "unresolved:parent_open_win32_2"
        );
        assert!(sink.events[0].payload["full_path"].is_null());
        assert_eq!(sink.events[1].id, engine::HEALTH);
        assert_eq!(sink.events[2].payload["unresolved_paths"], 1);
        sink.fail_id = Some(1000);
        let mut filter = FilteringSink::new(
            &mut sink,
            &policy,
            |_: &Event| Ok(ResolvedPath::unresolved("missing")),
            "fixture",
            "live",
        );
        assert!(filter.write(&Event::new(1000, json!({}))).is_err());
    }

    #[test]
    fn defaults_preserve_user_activity_and_match_only_complete_components() {
        let policy = PathFilters::default().compile().unwrap();
        for path in [
            r"\Windows\Logs\CBS\CBS.log",
            r"\WINDOWS\TEMP\file",
            r"\ProgramData\UsnEventLog\state\system.json",
        ] {
            assert_eq!(policy.decide(Some(path)).unwrap(), Decision::Blacklisted);
        }
        for path in [
            r"\Users\me\file.txt",
            r"\Users\me\AppData\Local\Temp\file",
            r"\Windows\System32\a.dll",
            r"\Windows\LogsBackup\a.log",
            r"\$Recycle.Bin\deleted",
            r"\ProgramData\UsnEventLog-other\a",
        ] {
            assert_eq!(policy.decide(Some(path)).unwrap(), Decision::Include);
        }
        assert_eq!(policy.decide(None).unwrap(), Decision::Include);
    }

    #[test]
    fn blacklist_wins_whitelist_and_unresolved_policy_is_explicit() {
        let policy = PathFilters {
            whitelist: vec![r"\Users".into()],
            blacklist: vec![r"\Users\me\cache".into()],
            unresolved: Unresolved::Exclude,
        }
        .compile()
        .unwrap();
        assert_eq!(
            policy.decide(Some(r"\Users\me\file")).unwrap(),
            Decision::Include
        );
        assert_eq!(
            policy.decide(Some(r"\users\ME\cache\file")).unwrap(),
            Decision::Blacklisted
        );
        assert_eq!(
            policy.decide(Some(r"\Windows\a")).unwrap(),
            Decision::NotWhitelisted
        );
        assert_eq!(policy.decide(None).unwrap(), Decision::Unresolved);
        assert_eq!(normalize_path("/Users/Me/").unwrap(), r"\users\me");
        for bad in [
            "",
            "file",
            r"C:\Users",
            r"\\server\share",
            r"\Users\..\Windows",
            r"\Users\\file",
            r"\Users\*",
            "\\a\0",
        ] {
            assert!(normalize_path(bad).is_err(), "{bad}");
        }
        let empty: PathFilters = serde_json::from_str(r#"{"blacklist":[]}"#).unwrap();
        assert_eq!(
            empty
                .compile()
                .unwrap()
                .decide(Some(r"\Windows\Logs\a"))
                .unwrap(),
            Decision::Include
        );
    }
}
