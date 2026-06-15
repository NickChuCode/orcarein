//! Board GPIO profiles — the *data* that makes one cdev backend serve many
//! single-board computers.
//!
//! M2 controls GPIO through the Linux character device (`/dev/gpiochipN`), which
//! is SoC-agnostic: the same code drives a Broadcom Pi and a Rockchip Orange Pi.
//! What differs between boards is *data*, not code:
//!
//!   * which gpiochip carries the user 40-pin header (resolved by kernel **label**
//!     at runtime — never a hard-coded chip number, since Pi 5's RP1 and RK3588's
//!     banks renumber across kernels), and
//!   * how a logical pin name ("LED") maps to a `(gpiochip, line offset)`.
//!
//! This module is the pure, always-compiled, hardware-free half: profile parsing,
//! label resolution, and the Rockchip `GPIOx_yz` → offset formula, all unit-tested
//! without a board. The `gpiocdev` I/O that opens the chip and drives the line
//! lands in a later cut and is verified on real hardware. See
//! `notes/specs/2026-06-15-orcarein-hardware-m2-m3-revised.md`.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;

use crate::error::HardwareError;

// ---------------------------------------------------------------------------
// gpiochip resolution by label
// ---------------------------------------------------------------------------

/// One GPIO chip as reported by the kernel (the `gpiodetect` view): its
/// `/dev/gpiochipN` index plus the driver-assigned label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipInfo {
    /// The `N` in `/dev/gpiochipN`.
    pub index: u32,
    /// The kernel label, e.g. `pinctrl-rp1` (Pi 5), `pinctrl-bcm2711` (Pi 4).
    pub label: String,
}

/// Picks the user-GPIO chip from the detected chips: the first chip whose label
/// contains one of `wanted` (substrings, tried in priority order). Returns its
/// `/dev/gpiochipN` index.
///
/// Resolving by label is mandatory: the user-facing chip number is not stable
/// (Pi 5 moved it between `gpiochip0` and `gpiochip4` across a 2024 kernel
/// reorder; RK3588 bank enumeration order isn't guaranteed either), but the
/// label is.
pub fn resolve_gpiochip_by_label(chips: &[ChipInfo], wanted: &[&str]) -> Option<u32> {
    wanted.iter().find_map(|needle| {
        chips
            .iter()
            .find(|c| c.label.contains(needle))
            .map(|c| c.index)
    })
}

// ---------------------------------------------------------------------------
// Rockchip banked-pin labels (GPIO{bank}_{port}{index})
// ---------------------------------------------------------------------------

/// A resolved Rockchip line: which GPIO bank it lives on, and the cdev line
/// offset within that bank's gpiochip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RkLine {
    /// GPIO bank (0..=4 on RK3588) — also the bank's gpiochip selector.
    pub bank: u32,
    /// Line offset within the bank: `port*8 + index` (A=0, B=1, C=2, D=3).
    pub offset: u32,
}

/// Parses a Rockchip pin label `GPIO{bank}_{port}{index}` (e.g. `GPIO1_C4`)
/// into its bank and cdev line offset. Each bank splits into ports A–D of 8
/// lines, so `offset = port*8 + index` (here `C4` → `2*8 + 4 = 20`). Returns
/// `None` for any malformed label (bad prefix, non-A–D port, index > 7).
pub fn parse_rk_line(label: &str) -> Option<RkLine> {
    let rest = label.strip_prefix("GPIO")?;
    let (bank_str, tail) = rest.split_once('_')?;
    let bank: u32 = bank_str.parse().ok()?;

    let mut chars = tail.chars();
    let port = chars.next()?;
    let port_idx: u32 = match port {
        'A' => 0,
        'B' => 1,
        'C' => 2,
        'D' => 3,
        _ => return None,
    };
    let index: u32 = chars.as_str().parse().ok()?;
    if index > 7 {
        return None;
    }
    Some(RkLine {
        bank,
        offset: port_idx * 8 + index,
    })
}

// ---------------------------------------------------------------------------
// Board profile model
// ---------------------------------------------------------------------------

/// How a logical pin resolves to a cdev line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinSpec {
    /// A line offset on the board's user-GPIO chip (resolved via
    /// [`BoardProfile::gpiochip_labels`]). Used for Pi-style boards where the
    /// whole 40-pin header sits on one pinctrl chip and the offset equals the
    /// BCM number.
    Offset(u32),
    /// A Rockchip banked line: the bank selects the gpiochip, the offset is the
    /// line within it. Used for RK3588-style boards whose pins span banks.
    Banked {
        /// GPIO bank (= bank's gpiochip selector).
        bank: u32,
        /// Line offset within the bank.
        offset: u32,
    },
}

/// A board's GPIO map: which chip carries the user header, and the named pins.
#[derive(Debug, Clone, PartialEq)]
pub struct BoardProfile {
    /// Board identifier, e.g. `rpi5`, `orangepi5plus`.
    pub name: String,
    /// Candidate kernel labels (priority order) for the user-GPIO chip. Used to
    /// resolve [`PinSpec::Offset`] pins at runtime. Empty for purely-banked
    /// boards whose every pin carries its own bank.
    pub gpiochip_labels: Vec<String>,
    /// Logical pin name → how it resolves to a cdev line.
    pub pins: BTreeMap<String, PinSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBoardProfile {
    schema_version: u32,
    board: RawBoard,
    #[serde(rename = "pin", default)]
    pins: Vec<RawPin>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBoard {
    name: String,
    #[serde(default)]
    gpiochip_labels: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPin {
    name: String,
    /// Offset on the board's user-GPIO chip (mutually exclusive with `line`).
    offset: Option<u32>,
    /// Rockchip banked label `GPIOx_yz` (mutually exclusive with `offset`).
    line: Option<String>,
}

impl BoardProfile {
    /// Parses and validates a TOML board profile. `HardwareError::Parse` for
    /// TOML syntax errors, `HardwareError::Validation` for semantic violations
    /// (unknown schema version, duplicate pin names, a pin with neither/both of
    /// `offset`/`line`, a malformed `line`, or an `offset` pin on a board with
    /// no `gpiochip_labels` to resolve its chip).
    pub fn from_toml_str(s: &str) -> Result<BoardProfile, HardwareError> {
        let raw: RawBoardProfile =
            toml::from_str(s).map_err(|e| HardwareError::Parse(e.to_string()))?;

        if raw.schema_version != 1 {
            return Err(HardwareError::Validation(format!(
                "unsupported schema_version {}; only version 1 is supported",
                raw.schema_version
            )));
        }

        let mut seen: HashSet<String> = HashSet::new();
        let mut pins: BTreeMap<String, PinSpec> = BTreeMap::new();
        for rp in raw.pins {
            if !seen.insert(rp.name.clone()) {
                return Err(HardwareError::Validation(format!(
                    "duplicate pin name {:?}",
                    rp.name
                )));
            }
            let spec = match (rp.offset, rp.line.as_deref()) {
                (Some(_), Some(_)) => {
                    return Err(HardwareError::Validation(format!(
                        "pin {:?}: both `offset` and `line` set; exactly one required",
                        rp.name
                    )));
                }
                (None, None) => {
                    return Err(HardwareError::Validation(format!(
                        "pin {:?}: neither `offset` nor `line` set; exactly one required",
                        rp.name
                    )));
                }
                (Some(off), None) => {
                    if raw.board.gpiochip_labels.is_empty() {
                        return Err(HardwareError::Validation(format!(
                            "pin {:?}: uses `offset` but board has no `gpiochip_labels` to \
                             resolve its chip",
                            rp.name
                        )));
                    }
                    PinSpec::Offset(off)
                }
                (None, Some(line)) => {
                    let rk = parse_rk_line(line).ok_or_else(|| {
                        HardwareError::Validation(format!(
                            "pin {:?}: malformed line label {:?} (expected GPIO{{bank}}_{{port}}{{index}})",
                            rp.name, line
                        ))
                    })?;
                    PinSpec::Banked {
                        bank: rk.bank,
                        offset: rk.offset,
                    }
                }
            };
            pins.insert(rp.name, spec);
        }

        Ok(BoardProfile {
            name: raw.board.name,
            gpiochip_labels: raw.board.gpiochip_labels,
            pins,
        })
    }

    /// Looks up a logical pin by name.
    pub fn pin(&self, name: &str) -> Option<PinSpec> {
        self.pins.get(name).copied()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn chips(pairs: &[(u32, &str)]) -> Vec<ChipInfo> {
        pairs
            .iter()
            .map(|(index, label)| ChipInfo {
                index: *index,
                label: (*label).to_string(),
            })
            .collect()
    }

    #[test]
    fn resolve_gpiochip_prefers_label_order_and_handles_renumbering() {
        // Pi 4: user GPIO on pinctrl-bcm2711.
        let pi4 = chips(&[(0, "pinctrl-bcm2711"), (1, "raspberrypi-exp-gpio")]);
        assert_eq!(
            resolve_gpiochip_by_label(&pi4, &["pinctrl-rp1", "pinctrl-bcm2711"]),
            Some(0)
        );

        // Pi 5, current kernel: user GPIO back on gpiochip0 / pinctrl-rp1.
        let pi5_new = chips(&[(0, "pinctrl-rp1"), (10, "gpio-brcmstb@107d508500")]);
        assert_eq!(
            resolve_gpiochip_by_label(&pi5_new, &["pinctrl-rp1", "pinctrl-bcm2711"]),
            Some(0)
        );

        // Pi 5, older image: same label, different index — label wins.
        let pi5_old = chips(&[(0, "gpio-brcmstb"), (4, "pinctrl-rp1")]);
        assert_eq!(
            resolve_gpiochip_by_label(&pi5_old, &["pinctrl-rp1"]),
            Some(4)
        );

        // No matching label → None (caller degrades / errors).
        let none = chips(&[(0, "something-else")]);
        assert_eq!(resolve_gpiochip_by_label(&none, &["pinctrl-rp1"]), None);
    }

    #[test]
    fn parse_rk_line_computes_bank_and_offset() {
        assert_eq!(
            parse_rk_line("GPIO1_C4"),
            Some(RkLine {
                bank: 1,
                offset: 20
            })
        );
        assert_eq!(
            parse_rk_line("GPIO0_A0"),
            Some(RkLine { bank: 0, offset: 0 })
        );
        assert_eq!(
            parse_rk_line("GPIO4_D7"),
            Some(RkLine {
                bank: 4,
                offset: 31
            })
        );
        assert_eq!(
            parse_rk_line("GPIO3_B5"),
            Some(RkLine {
                bank: 3,
                offset: 13
            })
        );
    }

    #[test]
    fn parse_rk_line_rejects_malformed() {
        assert_eq!(parse_rk_line("PA5"), None); // wrong prefix
        assert_eq!(parse_rk_line("GPIO1_E0"), None); // port E doesn't exist (A–D)
        assert_eq!(parse_rk_line("GPIO1_C8"), None); // index 8 > 7
        assert_eq!(parse_rk_line("GPIO1C4"), None); // missing underscore
        assert_eq!(parse_rk_line("GPIO_C4"), None); // missing bank
        assert_eq!(parse_rk_line("GPIO1_C"), None); // missing index
    }

    #[test]
    fn parses_offset_style_board() {
        let toml = r#"
schema_version = 1
[board]
name = "rpi5"
gpiochip_labels = ["pinctrl-rp1", "pinctrl-bcm2711"]
[[pin]]
name = "LED"
offset = 17
[[pin]]
name = "BUTTON"
offset = 27
"#;
        let b = BoardProfile::from_toml_str(toml).expect("valid offset board");
        assert_eq!(b.name, "rpi5");
        assert_eq!(b.gpiochip_labels, vec!["pinctrl-rp1", "pinctrl-bcm2711"]);
        assert_eq!(b.pin("LED"), Some(PinSpec::Offset(17)));
        assert_eq!(b.pin("BUTTON"), Some(PinSpec::Offset(27)));
        assert_eq!(b.pin("MISSING"), None);
    }

    #[test]
    fn parses_banked_style_board() {
        let toml = r#"
schema_version = 1
[board]
name = "orangepi5plus"
[[pin]]
name = "LED"
line = "GPIO1_C4"
[[pin]]
name = "RELAY"
line = "GPIO3_B5"
"#;
        let b = BoardProfile::from_toml_str(toml).expect("valid banked board");
        assert_eq!(b.name, "orangepi5plus");
        assert!(b.gpiochip_labels.is_empty());
        assert_eq!(
            b.pin("LED"),
            Some(PinSpec::Banked {
                bank: 1,
                offset: 20
            })
        );
        assert_eq!(
            b.pin("RELAY"),
            Some(PinSpec::Banked {
                bank: 3,
                offset: 13
            })
        );
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let bad = r#"
schema_version = 99
[board]
name = "x"
"#;
        let err = BoardProfile::from_toml_str(bad).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    #[test]
    fn rejects_duplicate_pin_names() {
        let bad = r#"
schema_version = 1
[board]
name = "x"
gpiochip_labels = ["pinctrl-rp1"]
[[pin]]
name = "LED"
offset = 1
[[pin]]
name = "LED"
offset = 2
"#;
        let err = BoardProfile::from_toml_str(bad).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    #[test]
    fn rejects_pin_with_both_offset_and_line() {
        let bad = r#"
schema_version = 1
[board]
name = "x"
gpiochip_labels = ["pinctrl-rp1"]
[[pin]]
name = "LED"
offset = 1
line = "GPIO1_C4"
"#;
        let err = BoardProfile::from_toml_str(bad).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    #[test]
    fn rejects_pin_with_neither_offset_nor_line() {
        let bad = r#"
schema_version = 1
[board]
name = "x"
gpiochip_labels = ["pinctrl-rp1"]
[[pin]]
name = "LED"
"#;
        let err = BoardProfile::from_toml_str(bad).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    #[test]
    fn rejects_offset_pin_without_gpiochip_labels() {
        // An offset pin needs a board-level chip to resolve against.
        let bad = r#"
schema_version = 1
[board]
name = "x"
[[pin]]
name = "LED"
offset = 1
"#;
        let err = BoardProfile::from_toml_str(bad).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    #[test]
    fn rejects_malformed_line_label() {
        let bad = r#"
schema_version = 1
[board]
name = "x"
[[pin]]
name = "LED"
line = "GPIO1_E0"
"#;
        let err = BoardProfile::from_toml_str(bad).unwrap_err();
        assert!(matches!(err, HardwareError::Validation(_)), "{err:?}");
    }

    // The shipped board-profile fixtures parse, and spot-checked pins resolve to
    // the values transcribed from the official manuals/whitepaper. These guard
    // against transcription and offset-formula errors (not against a wrong-but-
    // well-formed label — the final check is `gpio readall` on the board).
    #[test]
    fn shipped_board_profiles_parse() {
        // --- Raspberry Pi: offset == BCM on the user pinctrl chip ---
        let rpi4 = BoardProfile::from_toml_str(include_str!("../profiles/boards/rpi4.toml"))
            .expect("rpi4.toml");
        assert_eq!(rpi4.name, "rpi4");
        assert!(rpi4.gpiochip_labels.iter().any(|l| l.contains("bcm2711")));
        // Physical pin 11 = BCM17, pin 3 = BCM2 (SDA1), pin 19 = BCM10 (MOSI).
        assert_eq!(rpi4.pin("pin11"), Some(PinSpec::Offset(17)));
        assert_eq!(rpi4.pin("pin3"), Some(PinSpec::Offset(2)));
        assert_eq!(rpi4.pin("pin19"), Some(PinSpec::Offset(10)));

        let rpi5 = BoardProfile::from_toml_str(include_str!("../profiles/boards/rpi5.toml"))
            .expect("rpi5.toml");
        assert!(rpi5.gpiochip_labels.iter().any(|l| l.contains("rp1")));
        // Pi 5 shares the Pi 4 pinout; only the chip label differs.
        assert_eq!(rpi5.pin("pin11"), Some(PinSpec::Offset(17)));

        // --- Orange Pi 5 Plus (RK3588, banked) ---
        let plus =
            BoardProfile::from_toml_str(include_str!("../profiles/boards/orangepi5plus.toml"))
                .expect("orangepi5plus.toml");
        assert_eq!(plus.name, "orangepi5plus");
        assert!(plus
            .pins
            .values()
            .all(|p| matches!(p, PinSpec::Banked { .. })));
        // pin 16 = GPIO3_B5 → chip3 offset13; pin 3 = GPIO0_C0 → chip0 offset16.
        assert_eq!(
            plus.pin("pin16"),
            Some(PinSpec::Banked {
                bank: 3,
                offset: 13
            })
        );
        assert_eq!(
            plus.pin("pin3"),
            Some(PinSpec::Banked {
                bank: 0,
                offset: 16
            })
        );
        // The self-conflicting pins 8/10 are deliberately not shipped.
        assert_eq!(plus.pin("pin8"), None);
        assert_eq!(plus.pin("pin10"), None);

        // --- Orange Pi 5 Max (RK3588, banked, DIFFERENT pinout) ---
        let max = BoardProfile::from_toml_str(include_str!("../profiles/boards/orangepi5max.toml"))
            .expect("orangepi5max.toml");
        assert_eq!(max.name, "orangepi5max");
        assert!(max
            .pins
            .values()
            .all(|p| matches!(p, PinSpec::Banked { .. })));
        // pin 12 = GPIO4_A6 → chip4 offset6 (the 5 Max breaks out bank 4);
        // pin 31 = GPIO3_B5 → chip3 offset13.
        assert_eq!(
            max.pin("pin12"),
            Some(PinSpec::Banked { bank: 4, offset: 6 })
        );
        assert_eq!(
            max.pin("pin31"),
            Some(PinSpec::Banked {
                bank: 3,
                offset: 13
            })
        );
        // 5 Plus and 5 Max genuinely differ: pin 7 maps to different lines.
        assert_ne!(plus.pin("pin7"), max.pin("pin7"));
    }
}
