//! Windows native adapters. Unsafe API calls stay behind these safe interfaces.

mod collector;
pub mod converter;
mod discovery;
pub mod event_log;
pub mod service;
mod volume;

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
