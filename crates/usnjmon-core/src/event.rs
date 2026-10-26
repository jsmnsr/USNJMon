//! Internal event envelope and destination contract shared by collection and conversion.

use anyhow::Result;
use serde_json::{Value, json};

pub const COVERAGE: u32 = 101;
pub const GAP: u32 = 102;
pub const CAUGHT_UP: u32 = 103;
pub const HEALTH: u32 = 104;
pub const FATAL: u32 = 105;
pub const LIFECYCLE: u32 = 106;
pub const FILTER_SUMMARY: u32 = 107;

#[derive(Clone, Debug)]
pub struct Event {
    pub id: u32,
    pub payload: Value,
}

impl Event {
    #[must_use]
    pub fn new(id: u32, mut payload: Value) -> Self {
        payload["schema_version"] = json!(1);
        Self { id, payload }
    }
}

pub trait Sink {
    fn write(&mut self, event: &Event) -> Result<()>;
    fn finish_batch(&mut self) -> Result<()> {
        Ok(())
    }
}
