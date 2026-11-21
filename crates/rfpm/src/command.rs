//! The commands the meter takes, built so that only well-formed ones can be sent.
//!
//! The meter's parser is fragile: a short `A` command (without the attenuation) corrupts its
//! state (every reading becomes -99.9 dBm), a stray byte before a command makes it ignored, and
//! commands chained in one write fail. So [`Command::bytes`] checks the exact shape, and
//! [`crate::Commander`] sends one command per write, at least [`COMMAND_SPACING`] apart.

use std::fmt;
use std::time::Duration;

use crate::parse::Settings;

/// The least time between two commands: the meter's parser needs a beat.
pub const COMMAND_SPACING: Duration = Duration::from_millis(300);

/// The sample periods of `K01` … `K18` in µs, from the protocol notes (pairs are the same rate).
const SAMPLE_PERIODS_US: [u32; 18] = [
    2, 4, 8, 16, 32, 64, 128, 256, 256, 512, 512, 1024, 2048, 4096, 4096, 8192, 8192, 16384,
];

/// How long the meter takes to send a block of 500 records, during which it doesn't sample:
/// the gap between two blocks. Measured on a native-USB unit at K02 to K10 (every interval
/// was the block's span plus 148 ms); the protocol notes give ~158 ms for a unit with a
/// USB-UART bridge at 460800 baud.
pub const BLOCK_TRANSFER: Duration = Duration::from_millis(148);

/// A sample rate: `K01` (2 µs between samples, a block of 500 spans 1 ms) to `K18` (16 ms, a
/// block spans 8.2 s). The meter never reports it; it starts around `K01`/`K02`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SampleRate(u8);

impl SampleRate {
    /// Every sample rate, fastest first.
    pub fn all() -> impl Iterator<Item = Self> {
        (1..=18).map(Self)
    }

    /// The rates with a period of their own, fastest first: of the pairs that are the same
    /// rate (`K08`/`K09`, …) only the first.
    pub fn distinct() -> impl Iterator<Item = Self> {
        Self::all().filter(|rate| rate.0 == 1 || Self(rate.0 - 1).period() != rate.period())
    }

    /// The rate `K<code>`, for a code from 1 to 18.
    pub fn new(code: u8) -> Option<Self> {
        (1..=18).contains(&code).then_some(Self(code))
    }

    /// The code, 1 to 18.
    pub fn code(self) -> u8 {
        self.0
    }

    /// The time between two samples in a block.
    pub fn period(self) -> Duration {
        Duration::from_micros(u64::from(SAMPLE_PERIODS_US[usize::from(self.0) - 1]))
    }

    /// The time a block of 500 samples spans.
    pub fn block(self) -> Duration {
        self.period() * crate::READINGS_PER_FRAME as u32
    }

    /// The share of time the meter samples, 0 to 1: a block's span over the span plus the
    /// [`BLOCK_TRANSFER`] gap after it. Under 1 % at `K01`, 98 % at `K18`. Bursts shorter than
    /// the gap can fall in it unseen.
    pub fn coverage(self) -> f64 {
        let block = self.block().as_secs_f64();
        block / (block + BLOCK_TRANSFER.as_secs_f64())
    }

    /// The time from one block to the next: its span plus the [`BLOCK_TRANSFER`] gap.
    pub fn interval(self) -> Duration {
        self.block() + BLOCK_TRANSFER
    }

    /// The distinct rates (see [`Self::distinct`]) whose [`Self::interval`] fits a measured
    /// time between blocks, fastest first; empty when none does. The fastest rates are close
    /// together (149 to 156 ms), so a measurement there fits several. A rate fits within 3 ms
    /// plus 4 % (USB timing, the transfer time varying a little).
    pub fn matching(interval: Duration) -> Vec<Self> {
        let measured = interval.as_secs_f64();
        Self::distinct()
            .filter(|rate| {
                let expected = rate.interval().as_secs_f64();
                (measured - expected).abs() <= 0.003 + 0.04 * expected
            })
            .collect()
    }
}

/// A share as a percentage, with a decimal below 10 %.
fn percent(share: f64) -> String {
    let percent = share * 100.0;
    if percent < 10.0 {
        format!("{percent:.1} %")
    } else {
        format!("{percent:.0} %")
    }
}

impl fmt::Display for SampleRate {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let period = self.period().as_micros();
        let block = self.block();
        let block = if block < Duration::from_secs(1) {
            format!("{} ms", block.as_millis())
        } else {
            format!("{:.1} s", block.as_secs_f64())
        };
        let coverage = percent(self.coverage());
        write!(
            f,
            "K{:02}: {period} µs, block {block}, covers {coverage}",
            self.0
        )
    }
}

/// A command for the meter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    /// `A<MHz:4><±dd.d>`: frequency (calibration) and attenuation. The meter pauses ~1 s.
    Set(Settings),
    /// `Read`: report the settings, in a block marker (an [`crate::Event::Settings`]).
    Read,
    /// `K<nn>`: the sample rate. The meter pauses ~1 s and never reports it back.
    SampleRate(SampleRate),
}

/// Why a command can't be sent.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandError {
    /// The frequency is outside 1 to 9999 MHz.
    Frequency(u16),
    /// The attenuation is outside -99.9 to +99.9 dB (or not a number).
    Attenuation(f64),
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Frequency(mhz) => write!(f, "frequency {mhz} MHz is outside 1 to 9999 MHz"),
            Self::Attenuation(db) => write!(f, "attenuation {db} dB is outside -99.9 to +99.9 dB"),
        }
    }
}

impl std::error::Error for CommandError {}

impl Command {
    /// The bytes to write, CRLF included, checked to have exactly the shape the meter takes.
    pub fn bytes(&self) -> Result<Vec<u8>, CommandError> {
        let text = match *self {
            Self::Set(Settings {
                frequency_mhz,
                attenuation_db,
            }) => {
                if !(1..=9999).contains(&frequency_mhz) {
                    return Err(CommandError::Frequency(frequency_mhz));
                }
                let tenths = (attenuation_db * 10.0).round();
                if !(-999.0..=999.0).contains(&tenths) {
                    return Err(CommandError::Attenuation(attenuation_db));
                }
                let sign = if tenths < 0.0 { '-' } else { '+' };
                let tenths = tenths.abs() as u32;
                format!(
                    "A{frequency_mhz:04}{sign}{:02}.{}\r\n",
                    tenths / 10,
                    tenths % 10
                )
            }
            Self::Read => "Read\r\n".to_owned(),
            Self::SampleRate(rate) => format!("K{:02}\r\n", rate.code()),
        };
        assert!(
            well_formed(text.as_bytes()),
            "malformed meter command {text:?}"
        );
        Ok(text.into_bytes())
    }
}

/// Whether `bytes` is one of the three commands, exactly: the last line of defence against
/// the short `A` form.
fn well_formed(bytes: &[u8]) -> bool {
    let digit = |byte: &u8| byte.is_ascii_digit();
    match bytes {
        [b'A', f @ .., sign, a0, a1, b'.', a2, b'\r', b'\n'] => {
            f.len() == 4
                && f.iter().all(digit)
                && matches!(sign, b'+' | b'-')
                && [a0, a1, a2].into_iter().all(digit)
        }
        b"Read\r\n" => true,
        [b'K', k0, k1, b'\r', b'\n'] => digit(k0) && digit(k1),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(frequency_mhz: u16, attenuation_db: f64) -> Result<Vec<u8>, CommandError> {
        Command::Set(Settings {
            frequency_mhz,
            attenuation_db,
        })
        .bytes()
    }

    #[test]
    fn set_always_has_both_fields() {
        assert_eq!(set(5880, 10.0).unwrap(), b"A5880+10.0\r\n");
        assert_eq!(set(800, 0.0).unwrap(), b"A0800+00.0\r\n");
        assert_eq!(set(1, -0.04).unwrap(), b"A0001+00.0\r\n");
        assert_eq!(set(9999, -99.9).unwrap(), b"A9999-99.9\r\n");
        assert_eq!(set(868, 30.25).unwrap(), b"A0868+30.3\r\n");
    }

    #[test]
    fn set_refuses_what_it_cant_write() {
        assert_eq!(set(0, 0.0), Err(CommandError::Frequency(0)));
        assert_eq!(set(10_000, 0.0), Err(CommandError::Frequency(10_000)));
        assert_eq!(set(800, 100.0), Err(CommandError::Attenuation(100.0)));
        assert!(set(800, f64::NAN).is_err());
    }

    #[test]
    fn rates_from_the_time_between_blocks() {
        let codes = |ms| {
            let rates = SampleRate::matching(Duration::from_millis(ms));
            rates.into_iter().map(SampleRate::code).collect::<Vec<_>>()
        };
        // Intervals measured on the meter at K02, K05, K07, K08 and K10; the fastest rates
        // can't be told apart.
        assert_eq!(codes(150), [1, 2, 3, 4]);
        assert_eq!(codes(164), [4, 5]);
        assert_eq!(codes(212), [7]);
        assert_eq!(codes(276), [8]);
        assert_eq!(codes(404), [10]);
        assert_eq!(codes(8340), [18]);
        assert!(codes(100).is_empty());
        assert!(codes(700).is_empty());
        for rate in SampleRate::distinct() {
            assert!(SampleRate::matching(rate.interval()).contains(&rate));
        }
    }

    #[test]
    fn coverage_grows_with_the_block() {
        let coverage = |code| (SampleRate::new(code).unwrap().coverage() * 1000.0).round() / 10.0;
        assert_eq!(coverage(1), 0.7);
        assert_eq!(coverage(5), 9.8);
        assert_eq!(coverage(8), 46.4);
        assert_eq!(coverage(13), 87.4);
        assert_eq!(coverage(18), 98.2);
        let all: Vec<f64> = SampleRate::all().map(SampleRate::coverage).collect();
        assert!(all.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn read_and_sample_rate() {
        assert_eq!(Command::Read.bytes().unwrap(), b"Read\r\n");
        let k2 = SampleRate::new(2).unwrap();
        assert_eq!(Command::SampleRate(k2).bytes().unwrap(), b"K02\r\n");
        assert_eq!(k2.period(), Duration::from_micros(4));
        assert_eq!(k2.block(), Duration::from_millis(2));
        assert_eq!(k2.to_string(), "K02: 4 µs, block 2 ms, covers 1.3 %");
        assert_eq!(
            SampleRate::new(18).unwrap().to_string(),
            "K18: 16384 µs, block 8.2 s, covers 98 %"
        );
        assert_eq!(SampleRate::new(0), None);
        assert_eq!(SampleRate::new(19), None);
        assert_eq!(SampleRate::all().count(), 18);
        let distinct: Vec<u8> = SampleRate::distinct().map(SampleRate::code).collect();
        assert_eq!(distinct, [1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 13, 14, 16, 18]);
    }

    #[test]
    fn the_short_form_never_passes() {
        assert!(!well_formed(b"A0010\r\n"));
        assert!(!well_formed(b"A0800+00.0\r"));
        assert!(!well_formed(b"\rRead\r\n"));
        assert!(!well_formed(b"AT\r\n"));
    }
}
