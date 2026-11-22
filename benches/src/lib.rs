//! # veloxdesk-benches
//!
//! Pipeline latency benchmarks and network impairment simulator.
//!
//! * [`impair`] — deterministic loss/latency/jitter/reorder/bandwidth model and an async
//!   UDP proxy to put between two QUIC endpoints (used by the `pipeline` binary and by
//!   `veloxdesk-tests`).
//! * [`stats`] — percentile/latency table helpers for bench reports.

pub mod impair;
pub mod stats;

pub use impair::{BandwidthCap, Direction, ImpairmentConfig, Impairer, LossModel, UdpImpairProxy, Verdict};
