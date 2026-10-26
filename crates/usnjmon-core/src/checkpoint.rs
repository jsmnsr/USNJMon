//! Durable cursor data and persistence contract; storage is supplied by the application.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub schema_version: u32,
    pub volume_identity: String,
    pub journal_id: u64,
    pub next_usn: i64,
    pub backfill_end: i64,
    pub caught_up: bool,
}

impl Checkpoint {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema_version == 1, "unsupported checkpoint schema");
        ensure!(
            !self.volume_identity.is_empty(),
            "empty checkpoint volume identity"
        );
        ensure!(
            self.next_usn >= 0 && self.backfill_end >= 0,
            "negative checkpoint USN"
        );
        Ok(())
    }

    pub fn validate_live(&self) -> Result<()> {
        self.validate()?;
        ensure!(
            self.next_usn >= self.backfill_end && self.caught_up,
            "invalid live checkpoint: cursor precedes its split boundary or lane is not live"
        );
        Ok(())
    }
}

pub trait Store {
    fn save(&mut self, checkpoint: &Checkpoint) -> Result<()>;
}
