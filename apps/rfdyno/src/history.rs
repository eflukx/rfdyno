//! The history: every block of the last [`KEEP`], with its statistics and samples, for the
//! scrollable history plot (and to look at any past block again).

use std::collections::VecDeque;
use std::time::Duration;

use std::fmt::Write as _;

use chrono::{DateTime, Local, NaiveDateTime, TimeDelta, TimeZone};
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
    /// The burst's RMS (only the readings near the peak, see [`rfpm::Frame::burst`]) in dBm,
    /// and its duty (the share of readings in it, 0 to 1).
    pub burst: f32,
    pub duty: f32,
    /// The readings in dBm.
    pub samples: Box<[f32]>,
    /// The meter's settings it was measured with.
    pub meta: Meta,
}

/// The meter's settings a block was measured with.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Meta {
    /// The frequency (calibration) in MHz.
    pub frequency_mhz: u16,
    /// The attenuation the meter added, in dB.
    pub attenuation_db: f32,
    /// The sample rate's code: 8 for `K08`.
    pub rate: u8,
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
    /// The settings recorded with each block pushed from now on.
    pub meta: Meta,
}

impl History {
    /// Adds a block sampled around `at` (its middle, seconds since start) over `span`.
    pub fn push(&mut self, at: f64, span: Duration, frame: &Frame) {
        let samples: Box<[f32]> = frame.readings.iter().map(|&dbm| dbm as f32).collect();
        let burst = frame.burst();
        self.entries.push_back(Entry {
            burst: burst.map_or(f32::NEG_INFINITY, |burst| burst.rms_dbm as f32),
            duty: burst.map_or(0.0, |burst| burst.duty as f32),
            at,
            span: span.as_secs_f64(),
            mean: frame.mean_dbm() as f32,
            log_mean: (frame.readings.iter().sum::<f64>() / frame.readings.len().max(1) as f64)
                as f32,
            min: frame.min_dbm() as f32,
            max: frame.max_dbm() as f32,
            samples,
            meta: self.meta,
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

    /// Replaces everything with `entries` (as [`from_csv`] reads them), in time order.
    pub fn replace(&mut self, mut entries: Vec<Entry>) {
        entries.sort_by(|a, b| a.at.total_cmp(&b.at));
        self.entries = entries.into();
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
/// second 0), the meter's settings (frequency, attenuation, rate), its
/// span, peak, RMS (power average), plain average of the dBm readings and minimum, then its
/// readings. Everything in dBm (ms for the span); comma-separated, decimal point.
pub fn to_csv<'a>(entries: impl IntoIterator<Item = &'a Entry>, start: DateTime<Local>) -> String {
    let entries: Vec<&Entry> = entries.into_iter().collect();
    let samples = entries
        .iter()
        .map(|entry| entry.samples.len())
        .max()
        .unwrap_or(0);
    let mut csv = String::from(CSV_HEADER);
    for i in 1..=samples {
        let _ = write!(csv, ",s{i}");
    }
    csv.push('\n');
    for entry in entries {
        let at = start + TimeDelta::milliseconds((entry.start() * 1000.0).round() as i64);
        let _ = write!(
            csv,
            "{},{},{:.1},K{:02},{:.3},{:.1},{:.2},{:.2},{:.1}",
            at.format("%Y-%m-%dT%H:%M:%S%.3f"),
            entry.meta.frequency_mhz,
            entry.meta.attenuation_db,
            entry.meta.rate,
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

/// The columns before the readings in [`to_csv`]'s rows.
const CSV_HEADER: &str = "time,freq_mhz,atten_db,rate,span_ms,peak_dbm,rms_dbm,avg_dbm,min_dbm";

/// The columns before the readings in exports from before the settings were recorded.
const CSV_HEADER_WITHOUT_SETTINGS: &str = "time,span_ms,peak_dbm,rms_dbm,avg_dbm,min_dbm";

/// Reads blocks back from CSV as [`to_csv`] writes it, timed in seconds since `start`.
/// Exports from before the settings columns read too, their settings unknown (zero).
pub fn from_csv(text: &str, start: DateTime<Local>) -> Result<Vec<Entry>, String> {
    let mut lines = text.lines();
    let header = lines.next().unwrap_or_default();
    let with_settings = header.starts_with(CSV_HEADER);
    if !with_settings && !header.starts_with(CSV_HEADER_WITHOUT_SETTINGS) {
        return Err("not an rfpm export (its first line differs)".to_owned());
    }
    let mut entries = Vec::new();
    for (number, line) in lines.enumerate().map(|(i, line)| (i + 2, line)) {
        if line.trim().is_empty() {
            continue;
        }
        let entry = csv_row(line, start, with_settings)
            .map_err(|error| format!("line {number}: {error}"))?;
        entries.push(entry);
    }
    Ok(entries)
}

/// One row of [`to_csv`] as an entry (`with_settings`: with the settings columns).
fn csv_row(line: &str, start: DateTime<Local>, with_settings: bool) -> Result<Entry, String> {
    let mut fields = line.split(',');
    let mut next = |name: &str| fields.next().ok_or_else(|| format!("no {name}"));
    let time = next("time")?;
    let naive = NaiveDateTime::parse_from_str(time, "%Y-%m-%dT%H:%M:%S%.f")
        .map_err(|error| format!("time {time:?}: {error}"))?;
    let sampled = Local
        .from_local_datetime(&naive)
        .earliest()
        .ok_or_else(|| format!("time {time:?} doesn't exist here"))?;
    let number = |name: &str, text: &str| {
        text.trim()
            .parse::<f64>()
            .map_err(|_| format!("{name} {text:?} isn't a number"))
    };
    let meta = if with_settings {
        let frequency_mhz = number("frequency", next("frequency")?)? as u16;
        let attenuation_db = number("attenuation", next("attenuation")?)? as f32;
        let rate_text = next("rate")?;
        let rate = rate_text
            .trim()
            .strip_prefix('K')
            .and_then(|code| code.parse::<u8>().ok())
            .ok_or_else(|| format!("rate {rate_text:?} isn't K01 to K18"))?;
        Meta {
            frequency_mhz,
            attenuation_db,
            rate,
        }
    } else {
        Meta::default()
    };
    let span = number("span", next("span")?)? / 1000.0;
    let max = number("peak", next("peak")?)? as f32;
    let mean = number("rms", next("rms")?)? as f32;
    let log_mean = number("avg", next("avg")?)? as f32;
    let min = number("min", next("min")?)? as f32;
    let samples = fields
        .filter(|field| !field.trim().is_empty())
        .map(|field| number("reading", field).map(|dbm| dbm as f32))
        .collect::<Result<Box<[f32]>, _>>()?;
    let since_start = (sampled - start).num_microseconds().unwrap_or(0) as f64 / 1e6;
    // Not in the file: worked out from the readings, as when measured.
    let readings = samples.iter().map(|&dbm| f64::from(dbm)).collect();
    let burst = Frame { readings }.burst();
    Ok(Entry {
        burst: burst.map_or(f32::NEG_INFINITY, |burst| burst.rms_dbm as f32),
        duty: burst.map_or(0.0, |burst| burst.duty as f32),
        at: since_start + span / 2.0,
        span,
        mean,
        min,
        max,
        log_mean,
        samples,
        meta,
    })
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
        let mut history = History {
            meta: Meta {
                frequency_mhz: 830,
                attenuation_db: 30.0,
                rate: 8,
            },
            ..History::default()
        };
        for at in [1.5, 2.25] {
            history.push(at, SPAN, &frame(&[-20.0, -10.0]));
        }
        let csv = to_csv(history.entries(), start);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "time,freq_mhz,atten_db,rate,span_ms,peak_dbm,rms_dbm,avg_dbm,min_dbm,s1,s2"
        );
        assert_eq!(
            lines[1],
            "2026-11-21T22:40:01.499,830,30.0,K08,2.000,-10.0,-12.60,-15.00,-20.0,-20.0,-10.0"
        );
        assert!(lines[2].starts_with("2026-11-21T22:40:02.249,"));
        assert_eq!(lines.len(), 3);
        assert_eq!(
            to_csv([], start),
            "time,freq_mhz,atten_db,rate,span_ms,peak_dbm,rms_dbm,avg_dbm,min_dbm\n"
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
    fn csv_reads_back() {
        use chrono::TimeZone;

        let start = Local.with_ymd_and_hms(2026, 11, 21, 22, 40, 0).unwrap();
        let mut history = History {
            meta: Meta {
                frequency_mhz: 900,
                attenuation_db: -2.5,
                rate: 13,
            },
            ..History::default()
        };
        history.push(
            1.5,
            Duration::from_millis(500),
            &frame(&[-20.5, -10.0, -30.2]),
        );
        history.push(
            4.0,
            Duration::from_millis(500),
            &frame(&[-60.0, -59.5, -61.0]),
        );
        let csv = to_csv(history.entries(), start);
        // Read with another start: the same clock times, other seconds.
        let later = start + TimeDelta::seconds(10);
        let read = from_csv(&csv, later).unwrap();
        assert_eq!(read.len(), 2);
        for (original, back) in history.entries().iter().zip(&read) {
            assert!((back.at - (original.at - 10.0)).abs() < 1e-3);
            assert!((back.span - original.span).abs() < 1e-6);
            assert!((back.mean - original.mean).abs() < 0.01);
            assert!((back.log_mean - original.log_mean).abs() < 0.01);
            assert_eq!((back.min, back.max), (original.min, original.max));
            assert_eq!(back.samples.len(), original.samples.len());
            assert_eq!(back.meta, original.meta);
            assert!((back.burst - original.burst).abs() < 0.01);
            assert_eq!(back.duty, original.duty);
        }
        assert_eq!(read[0].samples[2], -30.2);

        assert!(
            from_csv("a,b,c\n", start)
                .unwrap_err()
                .contains("not an rfpm export")
        );
        // An older export, without the settings columns.
        let old = format!(
            "{CSV_HEADER_WITHOUT_SETTINGS},s1\n2026-11-21T22:40:01.000,2.000,-1.0,-1.00,-1.00,-1.0,-1.0\n"
        );
        let old = from_csv(&old, start).unwrap();
        assert_eq!(old[0].meta, Meta::default());
        assert_eq!(&*old[0].samples, [-1.0]);
        let bad = format!("{CSV_HEADER}\n2026-11-21T22:40:01.000,830,0.0,K02,x,1,2,3,4\n");
        assert_eq!(
            from_csv(&bad, start).unwrap_err(),
            "line 2: span \"x\" isn't a number"
        );
        let mut replaced = History::default();
        replaced.replace(read.into_iter().rev().collect());
        assert!(replaced.entries()[0].at < replaced.entries()[1].at);
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
