//! GPIO live monitor — the second consumer of the `overlay` primitive.
//!
//! Reads a set of pins' levels and shows them as a live, auto-refreshing view
//! (interactive tty) or a one-shot snapshot (piped / `--once` / headless — the
//! SBC-as-a-service case). The monitor is generic over [`GpioSource`]; today the
//! only backend is [`DemoGpio`] (animated, no hardware), so it runs and demos
//! anywhere. When `orcarein-hardware` is published, an adapter wrapping its
//! `Transport` will implement `GpioSource` for real pin reads (M2).
//!
//! This whole module is behind the (non-default) `hardware` feature, so the
//! published binary stays clean of a hardware command.

use std::time::{SystemTime, UNIX_EPOCH};

/// A source of GPIO pin levels. Narrow on purpose (the monitor only needs to
/// *read* a pin), so any backend — demo, future rppal, a remote board — is a
/// one-method impl.
pub trait GpioSource {
    /// Reads one pin: `Ok(true)` = high, `Ok(false)` = low, `Err` = unreadable.
    fn read(&self, pin: u8) -> Result<bool, String>;
}

/// Reads every pin once, pairing each with its result (errors captured, not
/// fatal — a bad pin shows `ERR`, the rest still render).
fn read_all(source: &dyn GpioSource, pins: &[u8]) -> Vec<(u8, Result<bool, String>)> {
    pins.iter().map(|&p| (p, source.read(p))).collect()
}

/// Renders pin readings to plain text (one row per pin). Pure — drives both the
/// live overlay frame and the headless snapshot.
fn render_gpio(readings: &[(u8, Result<bool, String>)]) -> String {
    if readings.is_empty() {
        return "（没有要监控的引脚）\n".to_string();
    }
    let mut out = String::from("引脚电平：\n\n");
    for (pin, r) in readings {
        let cell = match r {
            Ok(true) => "HIGH ●".to_string(),
            Ok(false) => "LOW  ○".to_string(),
            Err(e) => format!("ERR  {e}"),
        };
        out.push_str(&format!("  GPIO {pin:>2}   {cell}\n"));
    }
    out
}

/// Demo backend for the monitor: pin levels animate over wall-clock time, so
/// the live refresh visibly moves with zero real hardware.
pub struct DemoGpio;

impl GpioSource for DemoGpio {
    fn read(&self, pin: u8) -> Result<bool, String> {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok((secs + pin as u64) % 2 == 0)
    }
}

/// Runs the monitor. Live auto-refreshing overlay on a capable interactive tty;
/// otherwise (piped / dumb / headless / `--once`, or a non-`tui` build) a single
/// snapshot printed in place.
pub fn run_monitor(
    source: &dyn GpioSource,
    pins: &[u8],
    interval_ms: u64,
    once: bool,
) -> std::io::Result<()> {
    #[cfg(feature = "tui")]
    {
        use std::io::IsTerminal;
        let capable = crate::overlay::overlay_capable(
            std::io::stdout().is_terminal(),
            std::env::var("TERM").ok().as_deref(),
        );
        if !once && capable {
            return run_live(
                source,
                pins,
                std::time::Duration::from_millis(interval_ms.max(100)),
            );
        }
    }
    #[cfg(not(feature = "tui"))]
    let _ = (interval_ms, once);

    // Headless / one-shot snapshot.
    let readings = read_all(source, pins);
    print!("{}", render_gpio(&readings));
    Ok(())
}

/// The live overlay loop: redraw on a timer, refreshing pin levels each tick;
/// `q`/Esc quits. Terminal restore is the overlay guard's job.
#[cfg(feature = "tui")]
fn run_live(
    source: &dyn GpioSource,
    pins: &[u8],
    interval: std::time::Duration,
) -> std::io::Result<()> {
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
    use ratatui::layout::{Constraint, Layout};
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Line;
    use ratatui::widgets::Paragraph;

    let (mut terminal, _guard) = crate::overlay::enter_overlay()?;
    let hint = format!(
        " GPIO 监控（demo 数据）  每 {}ms 刷新 · q 退出 ",
        interval.as_millis()
    );

    loop {
        let body = render_gpio(&read_all(source, pins));
        terminal.draw(|f| {
            let chunks =
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
            f.render_widget(Paragraph::new(body.as_str()), chunks[0]);
            let footer =
                Line::from(hint.as_str()).style(Style::default().add_modifier(Modifier::REVERSED));
            f.render_widget(Paragraph::new(footer), chunks[1]);
        })?;

        // Wait up to `interval` for a key; on timeout, loop to refresh data.
        if event::poll(interval)? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Release
                    && matches!(k.code, KeyCode::Char('q') | KeyCode::Esc)
                {
                    break;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Even; // high on even pins, low on odd
    impl GpioSource for Even {
        fn read(&self, pin: u8) -> Result<bool, String> {
            Ok(pin % 2 == 0)
        }
    }

    struct Broken;
    impl GpioSource for Broken {
        fn read(&self, _pin: u8) -> Result<bool, String> {
            Err("no such pin".to_string())
        }
    }

    #[test]
    fn read_all_pairs_each_pin_with_its_level() {
        let r = read_all(&Even, &[2, 3, 4]);
        assert_eq!(r, vec![(2, Ok(true)), (3, Ok(false)), (4, Ok(true))]);
    }

    #[test]
    fn read_all_captures_errors_without_aborting() {
        let r = read_all(&Broken, &[9]);
        assert_eq!(r, vec![(9, Err("no such pin".to_string()))]);
    }

    #[test]
    fn render_gpio_shows_high_low_and_err() {
        let s = render_gpio(&[
            (17, Ok(true)),
            (27, Ok(false)),
            (22, Err("oops".to_string())),
        ]);
        assert!(s.contains("GPIO 17") && s.contains("HIGH"));
        assert!(s.contains("GPIO 27") && s.contains("LOW"));
        assert!(s.contains("GPIO 22") && s.contains("ERR") && s.contains("oops"));
    }

    #[test]
    fn render_gpio_handles_no_pins() {
        assert!(render_gpio(&[]).contains("没有"));
    }
}
