//! Prints one line per block (mean, min and max in dBm) and the meter's settings.
//!
//! `cargo run -p rfpm --example stream [port] [MHz] [attenuation dB]`. Without a port it takes
//! the first meter found; with a frequency it sets the frequency and attenuation (default
//! 0 dB) first, then asks for the settings.

use std::time::{Duration, Instant};

use rfpm::{Command, Event, Settings};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut meter = match args.first().map(String::as_str) {
        Some(name) if name != "auto" => rfpm::Meter::open(name)?,
        _ => rfpm::Meter::open_first()?,
    };
    eprintln!("reading {}", meter.name());
    if let Some(frequency) = args.get(1) {
        let attenuation_db = args.get(2).map_or(Ok(0.0), |db| db.parse())?;
        let settings = Settings {
            frequency_mhz: frequency.parse()?,
            attenuation_db,
        };
        meter.send(Command::Set(settings))?;
        // The meter pauses about a second after a setting.
        std::thread::sleep(Duration::from_millis(1200));
    }
    meter.send(Command::Read)?;
    let mut asked = Instant::now();
    let mut retried = false;
    let mut answered = false;

    let mut frames = rfpm::FrameCollector::new();
    loop {
        meter.read(|event| {
            if let Event::Settings(settings) = event {
                answered = true;
                println!(
                    "settings: {} MHz, {:+.1} dB",
                    settings.frequency_mhz, settings.attenuation_db
                );
            }
            if let Some(frame) = frames.push(event) {
                println!(
                    "{:7.2} dBm  (min {:6.1}, max {:6.1}, {} readings)",
                    frame.mean_dbm(),
                    frame.min_dbm(),
                    frame.max_dbm(),
                    frame.readings.len()
                );
            }
        })?;
        // The first `Read` after a setting is sometimes swallowed: ask once more.
        if !answered && !retried && asked.elapsed() > Duration::from_millis(1500) {
            meter.send(Command::Read)?;
            asked = Instant::now();
            retried = true;
        }
    }
}
