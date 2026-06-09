//! Transport abstraction for the hardware layer.
//!
//! [`Transport`] abstracts the Linux char-device transport (rppal) so all
//! logic and tests compile and run on any platform.  The real Linux impl
//! (`transport::linux`, rppal-backed) arrives in a later task; [`MockTransport`]
//! stands in for tests and non-Linux hosts.

use async_trait::async_trait;

use crate::error::HardwareError;

// ---------------------------------------------------------------------------
// NativeOp — the fixed set of hardware operations
// ---------------------------------------------------------------------------

/// One native transport operation, named by a profile `[intent.native] op`.
///
/// The fixed variant set is the entire native surface — new ops mean a new
/// variant here, never a DSL escape hatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeOp {
    /// Probe the I2C bus; returns the responding addresses.
    I2cScan,
    /// Write one byte to a device register.
    I2cWriteReg {
        /// Register address.
        reg: u8,
        /// Byte to write.
        value: u8,
    },
    /// Read one byte from a device register.
    I2cReadReg {
        /// Register address.
        reg: u8,
    },
    /// Drive a GPIO pin high or low.
    GpioSet {
        /// GPIO pin number.
        pin: u8,
        /// `true` = drive high; `false` = drive low.
        high: bool,
    },
    /// Read a GPIO pin level.
    GpioRead {
        /// GPIO pin number.
        pin: u8,
    },
}

// ---------------------------------------------------------------------------
// Transport trait
// ---------------------------------------------------------------------------

/// Abstracts the Linux char-device transport so logic and tests run on any
/// platform.
///
/// The real impl (`transport::linux`, rppal) arrives in a later task;
/// [`MockTransport`] stands in for tests and non-Linux hosts.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Execute one native op, returning a human/model-readable result string
    /// (e.g. `"0x40"` for a scan, the byte value for a read, `"ok"` for a
    /// write/set).
    async fn exec(&self, op: NativeOp) -> Result<String, HardwareError>;
}

// ---------------------------------------------------------------------------
// Sub-modules
// ---------------------------------------------------------------------------

pub mod mock;
pub use mock::MockTransport;
