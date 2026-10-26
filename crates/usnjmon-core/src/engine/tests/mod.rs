use super::*;
use crate::checkpoint::{Checkpoint, Store};
use crate::event::{Event, GAP, Sink};
use crate::journal::{self, fixtures};
use crate::stop::Stop;
use anyhow::Result;
use anyhow::bail;
use std::time::Duration;

#[derive(Default)]
struct Memory {
    events: Vec<Event>,
    checkpoint: Option<Checkpoint>,
    fail: bool,
    fail_after: Option<usize>,
}
impl Sink for Memory {
    fn write(&mut self, event: &Event) -> Result<()> {
        if self.fail || self.fail_after == Some(self.events.len()) {
            bail!("injected Event Log failure");
        }
        self.events.push(event.clone());
        Ok(())
    }
}
impl Store for Memory {
    fn save(&mut self, cp: &Checkpoint) -> Result<()> {
        if self.fail {
            bail!("injected disk failure");
        }
        self.checkpoint = Some(cp.clone());
        Ok(())
    }
}
fn journal() -> Journal {
    Journal {
        id: 1,
        first_usn: 100,
        lowest_valid_usn: 80,
        next_usn: 164,
    }
}
fn checkpoint() -> Checkpoint {
    reconcile(
        None,
        "volume-a",
        &journal(),
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
    )
    .unwrap()
    .checkpoint
}

mod delivery;
mod lanes;
mod reconcile;
