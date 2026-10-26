//! Path selection policy and its event-sink adapter.
mod policy;
mod sink;

pub use policy::{Decision, PathFilters, Policy, Unresolved, normalize_path};
pub use sink::{FilteringSink, ResolvedPath};

#[cfg(test)]
mod tests;
