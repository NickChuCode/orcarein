//! Real GPIO backend over the Linux character device, via `gpiocdev`.
//!
//! SoC-agnostic: the same code drives a Broadcom Pi and a Rockchip Orange Pi,
//! because it rides the kernel cdev ABI rather than any chip's register map. A
//! [`BoardProfile`] supplies the board's pin map; [`resolve_line`] turns a
//! logical pin name into the concrete `(gpiochip, offset)` to open.
//!
//! Compiled only on `target_os = "linux"` behind the `hardware` feature (the
//! `gpiocdev` dependency is Linux-only). The pure resolution it builds on
//! ([`resolve_line`]) is tested on every platform; this I/O shell is exercised
//! by CI on Linux and verified on the author's boards.
//!
//! **Output persistence**: libgpiod reverts a line to its prior state when the
//! request is released, so a "set a pin and walk away" call would lose its
//! level. [`CdevGpio`] therefore *holds* each output request alive for the
//! lifetime of the struct — the level persists until the `CdevGpio` is dropped.

use std::collections::HashMap;
use std::path::Path;

use gpiocdev::line::Value;
use gpiocdev::Request;

use crate::board::{resolve_line, BoardProfile, ChipInfo};
use crate::error::HardwareError;

/// Map any `gpiocdev` error into a transport error.
fn te<E: std::fmt::Display>(e: E) -> HardwareError {
    HardwareError::Transport(e.to_string())
}

/// Parse the `N` out of a `/dev/gpiochipN` path.
fn chip_index_from_path(path: &Path) -> Option<u32> {
    path.file_name()?
        .to_str()?
        .strip_prefix("gpiochip")?
        .parse()
        .ok()
}

/// Enumerate the GPIO chips present on the running board as `(index, label)`
/// pairs — the input [`resolve_line`] needs to map a pin's chip by label.
fn detect_chips() -> Result<Vec<ChipInfo>, HardwareError> {
    let mut out = Vec::new();
    for path in gpiocdev::chip::chips().map_err(te)? {
        let index = match chip_index_from_path(&path) {
            Some(i) => i,
            None => continue,
        };
        let chip = gpiocdev::chip::Chip::from_path(&path).map_err(te)?;
        let info = chip.info().map_err(te)?;
        out.push(ChipInfo {
            index,
            label: info.label,
        });
    }
    Ok(out)
}

/// A board-aware GPIO backend: drive and read logical pins by name through the
/// Linux character device. Output requests are retained so set levels persist.
pub struct CdevGpio {
    profile: BoardProfile,
    chips: Vec<ChipInfo>,
    /// Held output requests, keyed by `(chip_index, offset)`, kept alive so the
    /// driven level survives past the `set` call.
    held: HashMap<(u32, u32), Request>,
}

impl CdevGpio {
    /// Build a backend for `profile`, detecting the board's gpiochips now.
    pub fn new(profile: BoardProfile) -> Result<Self, HardwareError> {
        let chips = detect_chips()?;
        Ok(Self {
            profile,
            chips,
            held: HashMap::new(),
        })
    }

    /// The gpiochips detected at construction (for diagnostics / `doctor`).
    pub fn chips(&self) -> &[ChipInfo] {
        &self.chips
    }

    /// Drive a logical pin high or low. The output request is held alive so the
    /// level persists; calling `set` again on the same pin reuses the request.
    pub fn set(&mut self, pin: &str, high: bool) -> Result<(), HardwareError> {
        let line = resolve_line(&self.profile, pin, &self.chips)?;
        let val = if high { Value::Active } else { Value::Inactive };
        let key = (line.chip_index, line.offset);
        if let Some(req) = self.held.get(&key) {
            req.set_value(line.offset, val).map_err(te)?;
        } else {
            let req = Request::builder()
                .on_chip(format!("/dev/gpiochip{}", line.chip_index))
                .with_line(line.offset)
                .as_output(val)
                .request()
                .map_err(te)?;
            self.held.insert(key, req);
        }
        Ok(())
    }

    /// Read a logical pin's level. Requests the line as input transiently (so it
    /// must not currently be held as a driven output by this backend).
    pub fn read(&self, pin: &str) -> Result<bool, HardwareError> {
        let line = resolve_line(&self.profile, pin, &self.chips)?;
        let req = Request::builder()
            .on_chip(format!("/dev/gpiochip{}", line.chip_index))
            .with_line(line.offset)
            .as_input()
            .request()
            .map_err(te)?;
        Ok(req.value(line.offset).map_err(te)? == Value::Active)
    }
}
