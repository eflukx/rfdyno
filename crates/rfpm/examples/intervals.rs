//! Sets a sample rate, then prints the time between blocks and the rates it fits.
//!
//! `cargo run -p rfpm --example intervals [port|auto] <K code> [blocks]`

use std::time::{Duration, Instant};

use rfpm::{Command, SampleRate};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut meter = match args.first().map(String::as_str) {
        Some(name) if name != "auto" => rfpm::Meter::open(name)?,
        _ => rfpm::Meter::open_first()?,
    };
    let rate = args
        .get(1)
        .and_then(|code| SampleRate::new(code.parse().ok()?))
        .ok_or("K code 1-18")?;
    let blocks: usize = args.get(2).map_or(Ok(12), |count| count.parse())?;
    meter.send(Command::SampleRate(rate))?;
    std::thread::sleep(Duration::from_millis(1500));
    println!(
        "{rate}, expected a block every {} ms",
        rate.interval().as_millis()
    );

    let mut frames = rfpm::FrameCollector::new();
    let mut last: Option<Instant> = None;
    let mut seen = 0;
    while seen < blocks {
        let mut arrived = Vec::new();
        meter.read(|event| arrived.extend(frames.push(event).map(|_| Instant::now())))?;
        for at in arrived {
            if let Some(last) = last {
                let interval = at - last;
                let fits: Vec<String> = SampleRate::matching(interval)
                    .iter()
                    .map(|rate| format!("K{:02}", rate.code()))
                    .collect();
                println!("{:5} ms  fits {}", interval.as_millis(), fits.join(" "));
                seen += 1;
            }
            last = Some(at);
        }
    }
    Ok(())
}
