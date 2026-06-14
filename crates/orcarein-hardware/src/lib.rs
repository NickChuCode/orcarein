//! OrcaRein hardware control terminal layer.
//!
//! Loads declarative device *profiles* and exposes each profile intent as an
//! `orcarein_core::Tool`, dispatching to either native Rust transport
//! (`target_os = "linux"`, behind the `hardware` feature) or a persistent
//! Python sidecar for complex drivers. See
//! `notes/specs/2026-06-09-orcarein-hardware-terminal-positioning.md`.

pub mod board;
pub mod error;
pub mod profile;
pub mod sidecar;
pub mod tool;
pub mod transport;

pub use board::{
    parse_rk_line, resolve_gpiochip_by_label, BoardProfile, ChipInfo, PinSpec, RkLine,
};
pub use error::HardwareError;
pub use profile::{Backend, Device, Intent, Param, ParamType, Profile, Risk, TransportKind};
pub use sidecar::Sidecar;
pub use tool::{registry_from_profile, Executor, ProfileTool};
pub use transport::{MockTransport, NativeOp, Transport};
