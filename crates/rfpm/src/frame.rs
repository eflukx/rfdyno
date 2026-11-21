//! Frames: the readings between two `Aa` markers, with their statistics.

use crate::parse::{Event, READINGS_PER_FRAME};

/// Converts dBm to milliwatts.
pub fn dbm_to_mw(dbm: f64) -> f64 {
    10f64.powf(dbm / 10.0)
}

/// Converts milliwatts to dBm.
fn mw_to_dbm(mw: f64) -> f64 {
    10.0 * mw.log10()
}

/// The readings of one frame (normally [`READINGS_PER_FRAME`]), in dBm as the meter sent them.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// The readings, oldest first.
    pub readings: Vec<f64>,
}

impl Frame {
    /// The mean power over the frame in dBm, averaged as power (mW), not as dB, so bursts
    /// count for what they carry. `-inf` for an empty frame.
    pub fn mean_dbm(&self) -> f64 {
        if self.readings.is_empty() {
            return f64::NEG_INFINITY;
        }
        let sum: f64 = self.readings.iter().map(|&dbm| dbm_to_mw(dbm)).sum();
        mw_to_dbm(sum / self.readings.len() as f64)
    }

    /// The lowest reading in dBm, `+inf` for an empty frame.
    pub fn min_dbm(&self) -> f64 {
        self.readings.iter().copied().fold(f64::INFINITY, f64::min)
    }

    /// The highest reading in dBm, `-inf` for an empty frame.
    pub fn max_dbm(&self) -> f64 {
        self.readings
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max)
    }

    /// Whether the frame has all its readings (the first one after connecting usually hasn't).
    pub fn is_complete(&self) -> bool {
        self.readings.len() == READINGS_PER_FRAME
    }
}

/// The average waveform of `frames`: per sample position, the mean power in dBm (averaged as
/// power, like [`Frame::mean_dbm`]) over the frames that reach that far. As long as the
/// longest frame; empty for no frames.
pub fn mean_waveform<'a>(frames: impl IntoIterator<Item = &'a Frame>) -> Vec<f64> {
    let mut sums: Vec<(f64, u32)> = Vec::new();
    for frame in frames {
        if sums.len() < frame.readings.len() {
            sums.resize(frame.readings.len(), (0.0, 0));
        }
        for (sum, &dbm) in sums.iter_mut().zip(&frame.readings) {
            sum.0 += dbm_to_mw(dbm);
            sum.1 += 1;
        }
    }
    sums.into_iter()
        .map(|(mw, count)| mw_to_dbm(mw / f64::from(count)))
        .collect()
}

/// The peak waveform: per sample position, the highest reading in dBm since it was cleared.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PeakHold {
    peaks: Vec<f64>,
}

impl PeakHold {
    /// An empty peak hold.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the frame's readings into the peaks.
    pub fn push(&mut self, frame: &Frame) {
        if self.peaks.len() < frame.readings.len() {
            self.peaks.resize(frame.readings.len(), f64::NEG_INFINITY);
        }
        for (peak, &dbm) in self.peaks.iter_mut().zip(&frame.readings) {
            *peak = peak.max(dbm);
        }
    }

    /// The peaks in dBm, per sample position (as long as the longest frame taken).
    pub fn peaks(&self) -> &[f64] {
        &self.peaks
    }

    /// Forgets the peaks.
    pub fn clear(&mut self) {
        self.peaks.clear();
    }
}

/// Groups [`Event`]s into [`Frame`]s: a frame is done at the next `Aa`.
#[derive(Debug, Default)]
pub struct FrameCollector {
    readings: Vec<f64>,
    started: bool,
}

impl FrameCollector {
    /// A collector that drops readings until the first frame marker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one event, returning the frame it completes, if any. Ignores settings replies.
    pub fn push(&mut self, event: Event) -> Option<Frame> {
        match event {
            Event::Reading(dbm) => {
                if self.started {
                    self.readings.push(dbm);
                }
                None
            }
            Event::FrameStart => {
                let was_started = std::mem::replace(&mut self.started, true);
                let readings =
                    std::mem::replace(&mut self.readings, Vec::with_capacity(READINGS_PER_FRAME));
                (was_started && !readings.is_empty()).then_some(Frame { readings })
            }
            Event::Settings(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_is_a_power_average() {
        // 0 dBm (1 mW) and -inf-ish average to 0.5 mW = -3.01 dBm, not -50 dBm.
        let frame = Frame {
            readings: vec![0.0, -100.0],
        };
        assert!((frame.mean_dbm() - -3.0103).abs() < 1e-3);
        assert_eq!(frame.min_dbm(), -100.0);
        assert_eq!(frame.max_dbm(), 0.0);
    }

    #[test]
    fn mean_waveform_per_position() {
        let a = Frame {
            readings: vec![0.0, -10.0, -20.0],
        };
        let b = Frame {
            readings: vec![-100.0, -10.0],
        };
        let mean = mean_waveform([&a, &b]);
        assert_eq!(mean.len(), 3);
        assert!((mean[0] - -3.0103).abs() < 1e-3); // 1 mW and ~0: half a mW
        assert!((mean[1] - -10.0).abs() < 1e-9);
        assert!((mean[2] - -20.0).abs() < 1e-9); // only `a` reaches this far
        assert!(mean_waveform([]).is_empty());
    }

    #[test]
    fn peak_hold_per_position() {
        let mut hold = PeakHold::new();
        hold.push(&Frame {
            readings: vec![-50.0, -20.0],
        });
        hold.push(&Frame {
            readings: vec![-30.0, -40.0, -60.0],
        });
        assert_eq!(hold.peaks(), [-30.0, -20.0, -60.0]);
        hold.clear();
        assert!(hold.peaks().is_empty());
    }

    #[test]
    fn frames_run_from_marker_to_marker() {
        let mut collector = FrameCollector::new();
        let mut frames = Vec::new();
        let events = [
            Event::Reading(-1.0), // before the first marker: dropped
            Event::FrameStart,
            Event::Reading(-2.0),
            Event::Reading(-3.0),
            Event::FrameStart,
            Event::Reading(-4.0),
        ];
        for event in events {
            frames.extend(collector.push(event));
        }
        assert_eq!(
            frames,
            [Frame {
                readings: vec![-2.0, -3.0]
            }]
        );
        assert!(!frames[0].is_complete());
    }
}
