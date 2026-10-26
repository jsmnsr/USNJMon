//! Application configuration, filesystem persistence and Windows adapters.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

pub mod config;
#[cfg(any(windows, test))]
mod state;

#[cfg(windows)]
pub mod windows;
