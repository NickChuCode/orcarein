//! On-board verification for the cdev GPIO backend: blink a logical pin.
//!
//! Linux + `hardware` feature only (it needs the `gpiocdev` backend). On other
//! targets it just prints a notice so the example still builds.
//!
//! Run on a Pi / Orange Pi (the author's boards):
//!
//! ```text
//! cargo run -p orcarein-hardware --example blink --features hardware \
//!     -- crates/orcarein-hardware/profiles/boards/rpi5.toml pin11
//! ```
//!
//! Args: <board-profile.toml> <pin-name>. Toggles the pin 5 times at 1 Hz.
//! Also a good moment to cross-check the shipped pinout with `gpio readall`.

#[cfg(all(target_os = "linux", feature = "hardware"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    use orcarein_hardware::transport::CdevGpio;
    use orcarein_hardware::BoardProfile;

    let mut args = std::env::args().skip(1);
    let profile_path = args
        .next()
        .ok_or("usage: blink <board-profile.toml> <pin-name>")?;
    let pin = args
        .next()
        .ok_or("usage: blink <board-profile.toml> <pin-name>")?;

    let toml = std::fs::read_to_string(&profile_path)?;
    let profile = BoardProfile::from_toml_str(&toml)?;
    let board = profile.name.clone();
    let mut gpio = CdevGpio::new(profile)?;

    println!(
        "board {board}: detected {} gpiochip(s); blinking {pin} 5×",
        gpio.chips().len()
    );
    for i in 1..=5 {
        gpio.set(&pin, true)?;
        println!("  {i}: high");
        std::thread::sleep(Duration::from_millis(500));
        gpio.set(&pin, false)?;
        println!("  {i}: low");
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

#[cfg(not(all(target_os = "linux", feature = "hardware")))]
fn main() {
    eprintln!("the blink example requires Linux + `--features hardware`");
}
