//! orcarein-eval — the research-track evaluation harness.
//!
//! Batch-runs agent tasks across cache/context strategies, aggregates per-task
//! cost & cache metrics, and exports CSV. Reuses the `orcarein-core` headless
//! engine; see the Week 1 design spec for scope.
//!
//! Module declarations are added incrementally, one per task below, so the
//! crate compiles green after every task (a `pub mod` for a not-yet-created
//! file would break `cargo test`).

pub mod task;
