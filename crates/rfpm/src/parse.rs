//! The meter's byte stream: 10-byte records, `Aa` block markers and `Read` replies.
//!
//! ```text
//! -58600000u    -58.6 dBm, 000.00 µW: sign, dBm × 10 (3 digits), power DDD.dd (5 digits), unit
//! +03600229m    +3.6 dBm, 002.29 mW: unit u = µW, m = mW, w = W, which also ends the record
//! Aa            a block of 500 records starts
//! AR0800+00.0a  a `Read` reply inside a block marker: 800 MHz, +0.0 dB
//! ```

/// Records the meter sends per block (between two `Aa` markers).
pub const READINGS_PER_FRAME: usize = 500;

/// The reading the meter sends when its attenuation state is corrupt: a short `A` command
/// (without the attenuation) does that; a full one, or a power cycle, undoes it.
pub const CORRUPT_DBM: f64 = -99.9;

/// One thing the meter sent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// `Aa`: a new block starts.
    FrameStart,
    /// One reading in dBm, calibrated by the meter for its frequency, attenuation included.
    Reading(f64),
    /// The reply to `Read`: the meter's settings.
    Settings(Settings),
}

/// The meter's settings, as set with `A<MHz><±dd.d>` and reported by `Read`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// The frequency in MHz that picks the meter's calibration (1 to 9999; 1 after power-up).
    pub frequency_mhz: u16,
    /// Attenuation in front of the meter in dB, which the meter adds to every reading.
    pub attenuation_db: f64,
}

/// Digits in a record: 3 for dBm × 10, 5 for the power.
const RECORD_DIGITS: usize = 8;

/// The longest text taken between a marker's `A` and `a` (a `Read` reply is 10 bytes).
const MAX_MARKER_TEXT: usize = 32;

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum State {
    /// Waiting for the end of a record or a marker, after opening or garbage.
    #[default]
    Unsynced,
    /// Between records.
    Idle,
    /// Inside a record, after its sign.
    Record,
    /// Saw `A`; collecting up to the `a`.
    Marker,
}

/// Turns the meter's bytes into [`Event`]s, byte by byte, so reads may split records anywhere.
///
/// Starts unsynced: the partial record the stream opens with is dropped, and so is everything
/// up to the next unit letter or marker after a byte that doesn't fit.
#[derive(Debug, Default)]
pub struct Parser {
    state: State,
    negative: bool,
    digits: Vec<u8>,
    marker_text: Vec<u8>,
}

impl Parser {
    /// A parser at the start of a stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds `bytes`, calling `emit` for each complete event.
    pub fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(Event)) {
        for &byte in bytes {
            self.push(byte, &mut emit);
        }
    }

    /// Feeds one byte, calling `emit` for the events it completes.
    fn push(&mut self, byte: u8, emit: &mut impl FnMut(Event)) {
        let is_unit = matches!(byte, b'u' | b'm' | b'w');
        match (self.state, byte) {
            (State::Unsynced, _) if is_unit => self.state = State::Idle,
            (State::Unsynced | State::Idle, b'A') => {
                self.state = State::Marker;
                self.marker_text.clear();
            }
            (State::Unsynced, _) => {}

            (State::Idle, b'+' | b'-') => {
                self.state = State::Record;
                self.negative = byte == b'-';
                self.digits.clear();
            }
            (State::Idle, b'\r' | b'\n') => {}

            (State::Record, b'0'..=b'9') if self.digits.len() < RECORD_DIGITS => {
                self.digits.push(byte - b'0');
            }
            (State::Record, _) if is_unit => {
                // A unit letter ends a record either way, so a short one costs only itself.
                self.state = State::Idle;
                if self.digits.len() == RECORD_DIGITS {
                    let tenths = self.digits[..3]
                        .iter()
                        .fold(0, |sum, &digit| sum * 10 + i32::from(digit));
                    let dbm = f64::from(tenths) / 10.0;
                    emit(Event::Reading(if self.negative { -dbm } else { dbm }));
                }
            }

            (State::Marker, b'a') => {
                self.state = State::Idle;
                if let Some(settings) = parse_settings(&self.marker_text) {
                    emit(Event::Settings(settings));
                }
                emit(Event::FrameStart);
            }
            (State::Marker, b'A') => {
                // A reply followed by a whole marker (`AR0800+00.0Aa`): take the reply, go on.
                if let Some(settings) = parse_settings(&self.marker_text) {
                    emit(Event::Settings(settings));
                }
                self.marker_text.clear();
            }
            (State::Marker, _) if self.marker_text.len() < MAX_MARKER_TEXT => {
                self.marker_text.push(byte);
            }

            _ => self.state = State::Unsynced,
        }
    }
}

/// Reads `R0800+00.0` (or with spaces, `R 0800 +00.0`) from a marker's text.
fn parse_settings(text: &[u8]) -> Option<Settings> {
    let start = text.iter().position(|&byte| byte == b'R')?;
    let compact: Vec<u8> = text[start + 1..]
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let [f0, f1, f2, f3, sign, a0, a1, b'.', a2, ..] = compact[..] else {
        return None;
    };
    let digits = [f0, f1, f2, f3, a0, a1, a2];
    if !digits.iter().all(u8::is_ascii_digit) || !matches!(sign, b'+' | b'-') {
        return None;
    }
    let value = |digits: &[u8]| {
        digits
            .iter()
            .fold(0, |sum, &digit| sum * 10 + u32::from(digit - b'0'))
    };
    let attenuation_db = f64::from(value(&[a0, a1, a2])) / 10.0;
    Some(Settings {
        frequency_mhz: value(&[f0, f1, f2, f3]) as u16,
        attenuation_db: if sign == b'-' {
            -attenuation_db
        } else {
            attenuation_db
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(chunks: &[&str]) -> Vec<Event> {
        let mut parser = Parser::new();
        let mut events = Vec::new();
        for chunk in chunks {
            parser.feed(chunk.as_bytes(), |event| events.push(event));
        }
        events
    }

    fn settings(frequency_mhz: u16, attenuation_db: f64) -> Event {
        Event::Settings(Settings {
            frequency_mhz,
            attenuation_db,
        })
    }

    #[test]
    fn drops_the_partial_record_it_opens_with() {
        // The start of a real capture.
        let events = parse(&["600000u-59400000u-59700000u"]);
        assert_eq!(events, [Event::Reading(-59.4), Event::Reading(-59.7)]);
    }

    #[test]
    fn reads_the_dbm_field_not_the_power() {
        // From the protocol notes: +3.6 dBm is 2.29 mW; -12.3 dBm is 58.88 µW.
        let events = parse(&["u+03600229m-12305888u+12300001w"]);
        assert_eq!(
            events,
            [
                Event::Reading(3.6),
                Event::Reading(-12.3),
                Event::Reading(12.3)
            ]
        );
    }

    #[test]
    fn reads_block_markers() {
        let events = parse(&["u-58900000u-59500000uAa-59200000u"]);
        assert_eq!(
            events,
            [
                Event::Reading(-58.9),
                Event::Reading(-59.5),
                Event::FrameStart,
                Event::Reading(-59.2),
            ]
        );
    }

    #[test]
    fn reads_settings_replies_in_a_marker() {
        // As this meter sent it (`AR0800+00.0Aa`), and as the protocol notes show it.
        let events = parse(&["u-60100000uAR0800+00.0Aa-60200000uAR 5880 -10.5a"]);
        assert_eq!(
            events,
            [
                Event::Reading(-60.1),
                settings(800, 0.0),
                Event::FrameStart,
                Event::Reading(-60.2),
                settings(5880, -10.5),
                Event::FrameStart,
            ]
        );
    }

    #[test]
    fn syncs_on_a_marker_too() {
        let events = parse(&["0000Aa-01500000u"]);
        assert_eq!(events, [Event::FrameStart, Event::Reading(-1.5)]);
    }

    #[test]
    fn records_may_split_anywhere() {
        let chunks = [
            "u-594", "00000u-", "5970000", "0uA", "R0800+0", "0.0a+0", "0500001m",
        ];
        assert_eq!(
            parse(&chunks),
            [
                Event::Reading(-59.4),
                Event::Reading(-59.7),
                settings(800, 0.0),
                Event::FrameStart,
                Event::Reading(0.5),
            ]
        );
    }

    #[test]
    fn resyncs_after_garbage() {
        // A wrong letter, a short record and a long one each cost only themselves.
        let events = parse(&["u-594x0000u-5970000u-59700000u-597000000u-01000000u"]);
        assert_eq!(events, [Event::Reading(-59.7), Event::Reading(-1.0)]);
    }

    #[test]
    fn a_runaway_marker_resyncs() {
        let long = format!("uA{}u-01000000u", "x".repeat(40));
        assert_eq!(parse(&[&long]), [Event::Reading(-1.0)]);
    }

    #[test]
    fn a_garbled_reply_is_just_a_marker() {
        assert_eq!(parse(&["uAR08x0+00.0a"]), [Event::FrameStart]);
    }

    #[test]
    fn the_corrupt_state_reads_as_such() {
        assert_eq!(parse(&["u-99900000u"]), [Event::Reading(CORRUPT_DBM)]);
    }
}
