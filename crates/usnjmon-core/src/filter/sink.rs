//! Resolve candidate paths and report filtered counts before checkpoint advancement.

use super::{Decision, Policy};
use crate::event::{self, Event, Sink};
use anyhow::Result;
use serde_json::json;

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
    sink: &'a mut S,
    policy: &'a Policy,
    resolve: R,
    volume: &'a str,
    lane: &'a str,
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
                event::FILTER_SUMMARY,
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
