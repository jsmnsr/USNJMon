//! Platform-independent reconciliation and at-least-once batch delivery.
mod delivery;
mod reconcile;

pub use delivery::{deliver, historical_batch};
pub use reconcile::{
    Journal, Plan, ReconcileContext, apply_plan, reconcile, reconcile_history, reconcile_live,
};

#[cfg(test)]
mod tests;
