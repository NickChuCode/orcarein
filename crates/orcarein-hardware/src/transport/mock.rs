//! `MockTransport` — a programmable [`Transport`] for tests and non-Linux hosts.
//!
//! Scripted read bytes are pushed into a FIFO queue; write/gpio ops are
//! recorded in call order; scan results are configurable.  Lets every
//! upper-layer test run without real hardware.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use super::{NativeOp, Transport};
use crate::error::HardwareError;

// ---------------------------------------------------------------------------
// Inner state held behind the Mutex
// ---------------------------------------------------------------------------

struct Inner {
    /// Every op executed, in call order (scan / read / write all recorded).
    recorded: Vec<NativeOp>,
    /// FIFO of bytes returned by read ops (`I2cReadReg` / `GpioRead`).
    read_queue: VecDeque<u8>,
    /// String returned for `I2cScan`.
    scan_result: String,
}

impl Inner {
    fn new() -> Self {
        Inner {
            recorded: Vec::new(),
            read_queue: VecDeque::new(),
            scan_result: "0x40".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// MockTransport
// ---------------------------------------------------------------------------

/// A [`Transport`] that replays pre-canned responses, suitable for tests and
/// non-Linux hosts where the real char-device transport is unavailable.
///
/// ### Usage
///
/// Push read bytes and examine recorded ops without any real hardware:
///
/// ```no_run
/// use orcarein_hardware::transport::{MockTransport, NativeOp, Transport};
///
/// # async fn example() {
/// let mock = MockTransport::new();
/// mock.push_read(0x42);
/// let result = mock.exec(NativeOp::I2cReadReg { reg: 0x10 }).await.unwrap();
/// assert_eq!(result, "66"); // 0x42 == 66
/// # }
/// ```
pub struct MockTransport {
    inner: Mutex<Inner>,
}

impl MockTransport {
    /// Creates a new `MockTransport` with default scan result `"0x40"` and
    /// empty read queue / recorded ops.
    pub fn new() -> Self {
        MockTransport {
            inner: Mutex::new(Inner::new()),
        }
    }

    /// Override the string returned for `I2cScan`.
    ///
    /// Consumes and returns `self` to allow builder-style chaining:
    /// `MockTransport::new().with_scan_result("0x20,0x40")`.
    pub fn with_scan_result(self, s: impl Into<String>) -> Self {
        self.inner.lock().unwrap().scan_result = s.into();
        self
    }

    /// Push one byte onto the read queue.  The next `I2cReadReg` or
    /// `GpioRead` call will pop and return it as a decimal string.
    pub fn push_read(&self, byte: u8) {
        self.inner.lock().unwrap().read_queue.push_back(byte);
    }

    /// Return a snapshot of all ops executed so far, in call order.
    pub fn recorded(&self) -> Vec<NativeOp> {
        self.inner.lock().unwrap().recorded.clone()
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        MockTransport::new()
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn exec(&self, op: NativeOp) -> Result<String, HardwareError> {
        let mut inner = self.inner.lock().unwrap();
        inner.recorded.push(op.clone());

        match op {
            NativeOp::I2cScan => Ok(inner.scan_result.clone()),
            NativeOp::I2cWriteReg { .. } | NativeOp::GpioSet { .. } => Ok("ok".to_string()),
            NativeOp::I2cReadReg { .. } | NativeOp::GpioRead { .. } => {
                match inner.read_queue.pop_front() {
                    Some(byte) => Ok(byte.to_string()),
                    None => Err(HardwareError::Transport(
                        "MockTransport: read_queue is empty".to_string(),
                    )),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (written before implementation per TDD — run first to confirm they
// compile only after Transport trait exists)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: run a single async expression under tokio current-thread.
    macro_rules! run {
        ($e:expr) => {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on($e)
        };
    }

    #[test]
    fn scan_returns_default_result() {
        let mock = MockTransport::new();
        let result = run!(mock.exec(NativeOp::I2cScan)).unwrap();
        assert_eq!(result, "0x40");
    }

    #[test]
    fn scan_result_overridable_with_builder() {
        let mock = MockTransport::new().with_scan_result("0x20,0x40");
        let result = run!(mock.exec(NativeOp::I2cScan)).unwrap();
        assert_eq!(result, "0x20,0x40");
    }

    #[test]
    fn write_reg_returns_ok_and_is_recorded() {
        let mock = MockTransport::new();
        let result = run!(mock.exec(NativeOp::I2cWriteReg {
            reg: 0x01,
            value: 0xFF
        }))
        .unwrap();
        assert_eq!(result, "ok");
        assert_eq!(
            mock.recorded(),
            vec![NativeOp::I2cWriteReg {
                reg: 0x01,
                value: 0xFF
            }]
        );
    }

    #[test]
    fn gpio_set_returns_ok_and_is_recorded() {
        let mock = MockTransport::new();
        let result = run!(mock.exec(NativeOp::GpioSet { pin: 5, high: true })).unwrap();
        assert_eq!(result, "ok");
        assert_eq!(
            mock.recorded(),
            vec![NativeOp::GpioSet { pin: 5, high: true }]
        );
    }

    #[test]
    fn read_reg_returns_queued_byte_as_string() {
        let mock = MockTransport::new();
        mock.push_read(0x42); // 66 decimal
        let result = run!(mock.exec(NativeOp::I2cReadReg { reg: 0x10 })).unwrap();
        assert_eq!(result, "66");
    }

    #[test]
    fn gpio_read_returns_queued_byte_as_string() {
        let mock = MockTransport::new();
        mock.push_read(1);
        let result = run!(mock.exec(NativeOp::GpioRead { pin: 3 })).unwrap();
        assert_eq!(result, "1");
    }

    #[test]
    fn read_empty_queue_returns_transport_error() {
        let mock = MockTransport::new();
        let err = run!(mock.exec(NativeOp::I2cReadReg { reg: 0x00 })).unwrap_err();
        assert!(
            matches!(err, HardwareError::Transport(_)),
            "expected Transport error, got {err:?}"
        );
    }

    #[test]
    fn interleaved_ops_recorded_in_order() {
        let mock = MockTransport::new();
        mock.push_read(0xAB);
        mock.push_read(0xCD);

        run!(mock.exec(NativeOp::I2cWriteReg {
            reg: 0x01,
            value: 0x10
        }))
        .unwrap();
        run!(mock.exec(NativeOp::I2cReadReg { reg: 0x02 })).unwrap();
        run!(mock.exec(NativeOp::I2cScan)).unwrap();
        run!(mock.exec(NativeOp::GpioSet {
            pin: 2,
            high: false
        }))
        .unwrap();
        run!(mock.exec(NativeOp::GpioRead { pin: 7 })).unwrap();

        let log = mock.recorded();
        assert_eq!(log.len(), 5);
        assert_eq!(
            log[0],
            NativeOp::I2cWriteReg {
                reg: 0x01,
                value: 0x10
            }
        );
        assert_eq!(log[1], NativeOp::I2cReadReg { reg: 0x02 });
        assert_eq!(log[2], NativeOp::I2cScan);
        assert_eq!(
            log[3],
            NativeOp::GpioSet {
                pin: 2,
                high: false
            }
        );
        assert_eq!(log[4], NativeOp::GpioRead { pin: 7 });
    }

    #[test]
    fn recorded_captures_scan_ops_too() {
        let mock = MockTransport::new();
        run!(mock.exec(NativeOp::I2cScan)).unwrap();
        run!(mock.exec(NativeOp::I2cScan)).unwrap();
        assert_eq!(mock.recorded(), vec![NativeOp::I2cScan, NativeOp::I2cScan]);
    }
}
