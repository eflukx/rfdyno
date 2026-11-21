//! The history: every block of the last [`KEEP`], with its statistics and samples, for the
//! scrollable history plot (and to look at any past block again).

use std::collections::VecDeque;
use std::time::Duration;

use std::fmt::Write as _;

use chrono::{DateTime, Local, TimeDelta};
use rfpm::{Frame, dbm_to_mw};

/// How long the history goes back. At the fastest rates (~7 blocks a second, 500 samples of
/// 4 bytes) 30 minutes is about 25 MB.
pub const KEEP: Duration = Duration::from_secs(30 * 60);

/// Gaps longer than a block's own span plus this break the history's lines (a disconnect, a
/// setting the meter paused for).
const GAP_MARGIN_S: f64 = 1.5;

/// One block in the history.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The middle of the time it was sampled, in seconds since the viewer started (see
    /// [`sampled_middle`]).
    pub at: f64,
    /// How long the block spans, in seconds (its sample rate).
    pub span: f64,
    /// Mean power (averaged as power: the RMS), lowest and highest reading, in dBm.
    pub mean: f32,
    pub min: f32,
    pub max: f32,
    /// The plain mean of the readings in dBm (a log average): below `mean` for bursts.
    pub log_mean: f32,
    /// The readings in dBm.
    pub samples: Box<[f32]>,
}

impl Entry {
    /// When its first sample was taken, in seconds since start.
    pub fn start(&self) -> f64 {
        self.at - self.span / 2.0
    }

    /// The time between its samples, in seconds.
    pub fn period(&self) -> f64 {
        self.span / self.samples.len().max(1) as f64
    }

    /// When sample `index` was taken, in seconds since start.
    pub fn sample_time(&self, index: usize) -> f64 {
        self.start() + index as f64 * self.period()
    }

    /// The sample taken nearest to `at`, if `at` is within the block.
    pub fn sample_at(&self, at: f64) -> Option<f32> {
        let index = ((at - self.start()) / self.period()).round();
        (index >= 0.0)
            .then(|| self.samples.get(index as usize).copied())
            .flatten()
    }

    /// How many of its samples were taken between `from` and `to`.
    pub fn samples_between(&self, from: f64, to: f64) -> usize {
        let last = self.samples.len() as f64 - 1.0;
        let first = ((from - self.start()) / self.period()).ceil().max(0.0);
        let end = ((to - self.start()) / self.period()).floor().min(last);
        if end >= first {
            (end - first) as usize + 1
        } else {
            0
        }
    }

    /// The block back as a [`Frame`].
    pub fn frame(&self) -> Frame {
        Frame {
            readings: self.samples.iter().map(|&dbm| f64::from(dbm)).collect(),
        }
    }
}

/// The middle of the time a block was sampled, from when it `arrived` (seconds since start) and
/// its `span`: the meter samples the block, then spends [`rfpm::BLOCK_TRANSFER`] sending it.
pub fn sampled_middle(arrived: f64, span: Duration) -> f64 {
    arrived - rfpm::BLOCK_TRANSFER.as_secs_f64() - span.as_secs_f64() / 2.0
}

/// What a stretch of history holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    /// Blocks in the stretch.
    pub blocks: usize,
    /// Mean power over the blocks (averaged as power), in dBm.
    pub mean: f64,
    /// The highest reading, in dBm.
    pub max: f64,
    /// The lowest reading, in dBm.
    pub min: f64,
}

/// Every block of the last [`KEEP`], oldest first.
#[derive(Debug, Default)]
pub struct History {
    entries: VecDeque<Entry>,
}

impl History {
    /// Adds a block sampled around `at` (its middle, seconds since start) over `span`.
    pub fn push(&mut self, at: f64, span: Duration, frame: &Frame) {
        let samples: Box<[f32]> = frame.readings.iter().map(|&dbm| dbm as f32).collect();
        self.entries.push_back(Entry {
            at,
            span: span.as_secs_f64(),
            mean: frame.mean_dbm() as f32,
            log_mean: (frame.readings.iter().sum::<f64>() / frame.readings.len().max(1) as f64)
                as f32,
            min: frame.min_dbm() as f32,
            max: frame.max_dbm() as f32,
            samples,
        });
        let keep_from = at - KEEP.as_secs_f64();
        while self
            .entries
            .front()
            .is_some_and(|entry| entry.at < keep_from)
        {
            self.entries.pop_front();
        }
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The blocks, oldest first.
    pub fn entries(&self) -> &VecDeque<Entry> {
        &self.entries
    }

    /// The blocks that arrived between `from` and `to` (seconds since start).
    pub fn between(&self, from: f64, to: f64) -> impl Iterator<Item = &Entry> {
        let start = self.entries.partition_point(|entry| entry.at < from);
        let end = self.entries.partition_point(|entry| entry.at <= to);
        self.entries.range(start..end.max(start))
    }

    /// What the blocks between `from` and `to` hold; `None` for none.
    pub fn stats(&self, from: f64, to: f64) -> Option<Stats> {
        let mut stats = Stats {
            blocks: 0,
            mean: 0.0,
            max: f64::NEG_INFINITY,
            min: f64::INFINITY,
        };
        let mut mw = 0.0;
        for entry in self.between(from, to) {
            stats.blocks += 1;
            mw += dbm_to_mw(f64::from(entry.mean));
            stats.max = stats.max.max(f64::from(entry.max));
            stats.min = stats.min.min(f64::from(entry.min));
        }
        (stats.blocks > 0).then(|| Stats {
            mean: 10.0 * (mw / stats.blocks as f64).log10(),
            ..stats
        })
    }

    /// The block that covers `at`: the nearest, if it's within its own span plus a margin (so a
    /// gap in the history has none).
    pub fn at(&self, at: f64) -> Option<&Entry> {
        self.nearest(at)
            .filter(|entry| (entry.at - at).abs() <= entry.span.max(0.5) + 0.5)
    }

    /// The block nearest to `at`.
    pub fn nearest(&self, at: f64) -> Option<&Entry> {
        let after = self.entries.partition_point(|entry| entry.at < at);
        let candidates = [after.checked_sub(1), Some(after)];
        candidates
            .into_iter()
            .flatten()
            .filter_map(|index| self.entries.get(index))
            .min_by(|a, b| (a.at - at).abs().total_cmp(&(b.at - at).abs()))
    }

    /// The blocks to draw for `from` to `to`: those between, plus the nearest one beyond each
    /// edge (so lines run off the edges however far apart blocks are), split where data stopped
    /// for a while, so lines don't bridge a disconnect.
    pub fn runs(&self, from: f64, to: f64) -> Vec<Vec<&Entry>> {
        let start = self
            .entries
            .partition_point(|entry| entry.at < from)
            .saturating_sub(1);
        let end =
            (self.entries.partition_point(|entry| entry.at <= to) + 1).min(self.entries.len());
        let mut runs: Vec<Vec<&Entry>> = Vec::new();
        let mut previous: Option<&Entry> = None;
        for entry in self.entries.range(start..end.max(start)) {
            let gap = previous.is_some_and(|previous| {
                entry.at - previous.at > entry.span.max(previous.span) + GAP_MARGIN_S
            });
            match runs.last_mut() {
                Some(run) if !gap => run.push(entry),
                _ => runs.push(vec![entry]),
            }
            previous = Some(entry);
        }
        runs
    }
}

/// `entries` as CSV, one row per block: the local time its sampling started (`start` is
/// second 0), its
/// span, peak, RMS (power average), plain average of the dBm readings and minimum, then its
/// readings. Everything in dBm (ms for the span); comma-separated, decimal point.
pub fn to_csv<'a>(entries: impl IntoIterator<Item = &'a Entry>, start: DateTime<Local>) -> String {
    let entries: Vec<&Entry> = entries.into_iter().collect();
    let samples = entries
        .iter()
        .map(|entry| entry.samples.len())
        .max()
        .unwrap_or(0);
    let mut csv = String::from("time,span_ms,peak_dbm,rms_dbm,avg_dbm,min_dbm");
    for i in 1..=samples {
        let _ = write!(csv, ",s{i}");
    }
    csv.push('\n');
    for entry in entries {
        let at = start + TimeDelta::milliseconds((entry.start() * 1000.0).round() as i64);
        let _ = write!(
            csv,
            "{},{:.3},{:.1},{:.2},{:.2},{:.1}",
            at.format("%Y-%m-%dT%H:%M:%S%.3f"),
            entry.span * 1000.0,
            entry.max,
            entry.mean,
            entry.log_mean,
            entry.min
        );
        for dbm in &entry.samples {
            let _ = write!(csv, ",{dbm:.1}");
        }
        csv.push('\n');
    }
    csv
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPAN: Duration = Duration::from_millis(2);

    fn frame(readings: &[f64]) -> Frame {
        Frame {
            readings: readings.to_vec(),
        }
    }

    fn history(times: &[f64]) -> History {
        let mut history = History::default();
        for &at in times {
            history.push(at, SPAN, &frame(&[-20.0, -10.0]));
        }
        history
    }

    #[test]
    fn keeps_only_the_last_half_hour() {
        let keep = KEEP.as_secs_f64();
        let history = history(&[0.0, 10.0, keep + 5.0]);
        let times: Vec<f64> = history.entries().iter().map(|entry| entry.at).collect();
        assert_eq!(times, [10.0, keep + 5.0]);
    }

    #[test]
    fn statistics_of_a_stretch() {
        let mut history = History::default();
        history.push(1.0, SPAN, &frame(&[0.0, 0.0])); // 1 mW
        history.push(2.0, SPAN, &frame(&[-100.0, -100.0])); // ~0 mW
        history.push(9.0, SPAN, &frame(&[10.0]));
        let stats = history.stats(0.5, 2.5).unwrap();
        assert_eq!(stats.blocks, 2);
        assert!((stats.mean - -3.0103).abs() < 1e-3);
        assert_eq!((stats.min, stats.max), (-100.0, 0.0));
        assert_eq!(history.stats(3.0, 8.0), None);
    }

    #[test]
    fn nearest_block() {
        let history = history(&[1.0, 2.0, 4.0]);
        assert_eq!(history.nearest(0.0).unwrap().at, 1.0);
        assert_eq!(history.nearest(2.9).unwrap().at, 2.0);
        assert_eq!(history.nearest(3.1).unwrap().at, 4.0);
        assert_eq!(history.nearest(99.0).unwrap().at, 4.0);
        assert!(History::default().nearest(1.0).is_none());
        // `at` only within a block's reach (span 2 ms, so 1 s).
        assert_eq!(history.at(2.9).unwrap().at, 2.0);
        assert_eq!(history.at(3.2).unwrap().at, 4.0);
        assert!(history.at(10.0).is_none());
    }

    #[test]
    fn runs_break_at_gaps() {
        let history = history(&[1.0, 1.2, 1.4, 5.0, 5.2]);
        let runs: Vec<Vec<f64>> = history
            .runs(0.0, 10.0)
            .into_iter()
            .map(|run| run.into_iter().map(|entry| entry.at).collect())
            .collect();
        assert_eq!(runs, [vec![1.0, 1.2, 1.4], vec![5.0, 5.2]]);
        // Zoomed in between blocks: the neighbours beyond both edges come along.
        let spread = self::history(&[1.0, 2.0, 3.0, 4.0]);
        let shown = |from, to| -> Vec<f64> {
            spread
                .runs(from, to)
                .into_iter()
                .flatten()
                .map(|entry| entry.at)
                .collect()
        };
        assert_eq!(shown(2.2, 2.8), [2.0, 3.0]);
        assert_eq!(shown(1.5, 3.5), [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(shown(4.5, 9.0), [4.0]);
        assert_eq!(shown(-5.0, -1.0), [1.0]);
    }

    #[test]
    fn csv_one_row_per_block() {
        use chrono::TimeZone;

        let start = Local.with_ymd_and_hms(2026, 11, 21, 22, 40, 0).unwrap();
        let history = history(&[1.5, 2.25]);
        let csv = to_csv(history.entries(), start);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "time,span_ms,peak_dbm,rms_dbm,avg_dbm,min_dbm,s1,s2"
        );
        assert_eq!(
            lines[1],
            "2026-11-21T22:40:01.499,2.000,-10.0,-12.60,-15.00,-20.0,-20.0,-10.0"
        );
        assert!(lines[2].starts_with("2026-11-21T22:40:02.249,"));
        assert_eq!(lines.len(), 3);
        assert_eq!(
            to_csv([], start),
            "time,span_ms,peak_dbm,rms_dbm,avg_dbm,min_dbm\n"
        );
    }

    #[test]
    fn sample_times() {
        // Arrived at 10 s, 2 ms span: sampled 148 + 2 ms before, the middle 1 ms in.
        assert!((sampled_middle(10.0, SPAN) - 9.851).abs() < 1e-9);
        let mut history = History::default();
        history.push(
            5.0,
            Duration::from_millis(500),
            &frame(&[-30.0, -20.0, -10.0, 0.0, 10.0]),
        );
        let entry = &history.entries()[0];
        assert_eq!(entry.start(), 4.75);
        assert_eq!(entry.period(), 0.1);
        assert!((entry.sample_time(2) - 4.95).abs() < 1e-9);
        assert_eq!(entry.sample_at(4.96), Some(-10.0));
        assert_eq!(entry.sample_at(4.6), None);
        assert_eq!(entry.sample_at(5.3), None);
        // Samples at 4.75, 4.85, … 5.15.
        assert_eq!(entry.samples_between(0.0, 10.0), 5);
        assert_eq!(entry.samples_between(4.8, 5.0), 2);
        assert_eq!(entry.samples_between(5.2, 6.0), 0);
        assert_eq!(entry.samples_between(4.0, 4.7), 0);
    }

    #[test]
    fn an_entry_gives_its_frame_back() {
        let history = history(&[1.0]);
        let entry = &history.entries()[0];
        assert_eq!(entry.frame(), frame(&[-20.0, -10.0]));
        assert_eq!((entry.min, entry.max), (-20.0, -10.0));
        // RMS (power average) of -20 and -10 dBm is -12.6; their plain mean -15.
        assert!((entry.mean - -12.596).abs() < 1e-3);
        assert_eq!(entry.log_mean, -15.0);
    }
}
