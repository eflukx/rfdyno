//! Driver for the cheap USB "RF-Power-Meter-V5.0" (100 kHz to 10 GHz, 0 to -60 dBm, USB-C).
//!
//! # How to use
//!
//! ```no_run
//! use rfpm::{Command, Settings};
//!
//! let mut meter = rfpm::Meter::open_first()?;
//! // The meter forgets its settings at power-up (1 MHz): set the band you measure.
//! meter.send(Command::Set(Settings { frequency_mhz: 800, attenuation_db: 30.0 }))?;
//! let mut frames = rfpm::FrameCollector::new();
//! loop {
//!     meter.read(|event| {
//!         if let Some(frame) = frames.push(event) {
//!             println!("{:.1} dBm", frame.mean_dbm());
//!         }
//!     })?;
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Working principle
//!
//! The meter is a log detector (AD8317/AD8318) read by an STM32F401, which shows up as a USB
//! serial port (`0483:5740`, "STM32 Virtual ComPort in FS Mode"). It streams as soon as the
//! port is open, about 3000 records a second, in blocks of 500 behind an `Aa` marker:
//!
//! ```text
//! Aa-59400000u-59700000u … (500 records) … Aa+03600229m …
//! ```
//!
//! Each record is the sign, dBm × 10 in 3 digits, the power as DDD.dd and its unit (`u` µW,
//! `m` mW, `w` W), which also ends the record. The meter computes the dBm itself, from its
//! calibration for the set frequency, attenuation included. A block is one burst of 500
//! samples at the set sample rate, then sent in ~150 ms.
//!
//! It takes three commands ([`Command`]): `A<MHz><±dd.d>` (frequency and attenuation), `Read`
//! (reply inside a block marker: `AR0800+00.0a`) and `K01`…`K18` (sample rate). Its parser is
//! fragile, see [`Command`]. Protocol notes: <https://github.com/LostInNovo/rf-power-meter-v5-companion>.
//!
//! [`Parser`] turns bytes into [`Event`]s, [`FrameCollector`] groups readings into [`Frame`]s,
//! [`Meter`] owns the port and [`Commander`] sends commands.
//!
//! The meter's descriptors lack an interface association: with Windows' own driver the port
//! splits in two (`MI_00`, `MI_01`) and only the data half opens; ST's VCP driver takes the
//! whole device. [`find_ports`] lists the control half last.

mod command;
mod frame;
mod parse;
mod port;

pub use command::{BLOCK_TRANSFER, COMMAND_SPACING, Command, CommandError, SampleRate};
pub use frame::{Frame, FrameCollector, PeakHold, dbm_to_mw, mean_waveform};
pub use parse::{CORRUPT_DBM, Event, Parser, READINGS_PER_FRAME, Settings};
pub use port::{Commander, Meter, MeterPort, PID, SendError, VID, find_ports};

/// The lowest power the meter measures, in dBm: readings near it are its noise floor.
pub const FLOOR_DBM: f64 = -60.0;

/// The highest power the meter is rated for, in dBm (it compresses above about -5 dBm).
pub const MAX_DBM: f64 = 0.0;

/// Above this the detector is damaged at once, in dBm.
pub const DAMAGE_DBM: f64 = 12.0;
