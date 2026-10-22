//! Windows native adapters. Unsafe API calls stay behind these safe interfaces.

pub mod collector;
pub mod converter;
mod discovery;
pub mod event_log;
pub mod service;
pub mod volume;
