//! Bounded EVTX 3.1 output with typed Binary XML; no provider or channel operations.
#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod binxml;
mod writer;

pub use binxml::{Element, Value};
pub use writer::EvtxWriter;
