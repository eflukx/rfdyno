//! RFdyno: shows what the USB "RF-Power-Meter-V5.0" measures.
//!
//! # How to use
//!
//! Plug in the meter and start `rfdyno`: it connects to the first meter it finds (or pick a
//! port at the top) and sets the meter's frequency, attenuation and sample rate, which the
//! meter forgets at every power-up. Set the frequency to the band you measure (it picks the
//! meter's calibration) and the attenuation to what sits in front of the meter (the meter adds
//! it to every reading), then Apply. With Auto-reconnect on, it reconnects by itself when the
//! meter is unplugged or stops sending, and sets it again.
//!
//! The big number is the mean power over the last few blocks (Average). The line under it
//! checks the sample rate: the meter never reports it, but the time between blocks shows it
//! (block span plus ~148 ms of sending), so it says which rate fits and warns when that isn't
//! the one sent (a meter power-cycled behind the viewer's back). Below it:
//!
//! - **Block**: the 500 samples of a block over time: the last one, the average over the
//!   averaged blocks and the peak hold since Reset peaks, each switchable. Hold freezes it.
//! - **History**: every block of the last 30 minutes, its mean on the band between its lowest
//!   and highest reading, at the time it was sampled; zoomed in, every sample. It follows the newest block (Live); dragging looks back, a click shows
//!   that block in the Block plot. The line above it sums up what's in view.
//!
//! - **Trigger**: every block whose RMS goes over the threshold is captured (the last 1000),
//!   marked in the history; the block plot then shows the last capture (‹ › step through
//!   them, Back to live runs live until the next one, Captures goes back), Export the captures.
//!
//! On both plots the wheel zooms the time axis and Ctrl+wheel the dB axis, dragging pans the
//! time axis, and a double click switches the dB axis between fitting the data and the full
//! range. The history's time axis is clock time.
//!
//! # Working principle
//!
//! A reader thread owns the port ([`link::Link`]) and sends each complete block and each
//! settings reply over a channel; the UI keeps the last blocks for the average and every block
//! in the [`history::History`], and repaints when something arrives. Commands go out on
//! threads of their own, so the meter's command spacing never blocks the UI. Reconnecting is
//! tried once a second while the link is down; a link that sends nothing for longer than a
//! block takes (plus the settling pauses) counts as down. Means are averaged as power (mW).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod history;
mod link;
mod plots;
mod theme;

use std::collections::VecDeque;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use egui::RichText;
use rfpm::{
    CORRUPT_DBM, DAMAGE_DBM, FLOOR_DBM, Frame, MAX_DBM, Meter, MeterPort, PeakHold, SampleRate,
    Settings, dbm_to_mw,
};

use crate::history::History;
use crate::link::{Applying, Link, Message, REPLY_TIMEOUT, SETTLE};
use crate::plots::{HistoryView, Trace, YScale};
use crate::theme::Theme;

/// What the port choice says to pick the first meter found.
const AUTO: &str = "Auto";

/// Readings within this of the floor count as "no signal".
const FLOOR_MARGIN_DB: f64 = 1.0;

/// The sample rate the meter starts at.
const DEFAULT_RATE: u8 = 2;

/// Blocks the rate check looks at (the last ones since a setting), and how many it needs.
const RATE_CHECK_BLOCKS: usize = 16;
const RATE_CHECK_MIN: usize = 5;

/// Readings this far from 0 (±99.9 dBm) are the meter's "no valid reading", not a level.
const INVALID_DBM: f64 = 99.85;

/// The most blocks the trigger keeps (about 2 kB each).
const MAX_CAPTURES: usize = 1000;

/// How long the history bar shows what an export or open did.
const NOTE_FOR: Duration = Duration::from_secs(8);

/// How often to try reconnecting.
const RECONNECT_EVERY: Duration = Duration::from_secs(1);

/// Silence beyond a block's span (and the settling pauses) after which the link counts as down.
const STALL_MARGIN: Duration = Duration::from_secs(3);

/// The history windows to choose from, in seconds.
const WINDOWS: [(f64, &str); 6] = [
    (3.0, "3 s"),
    (10.0, "10 s"),
    (60.0, "1 min"),
    (300.0, "5 min"),
    (600.0, "10 min"),
    (1800.0, "30 min"),
];

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RFdyno")
            .with_icon(icon())
            .with_inner_size([900.0, 760.0])
            .with_min_inner_size([480.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "rfdyno",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

/// The window's icon: 64 x 64 RGBA, drawn by `tools/icon.py` (as the executable's).
fn icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../assets/icon-64.rgba").to_vec(),
        width: 64,
        height: 64,
    }
}

/// The block plot's traces, each on or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Traces {
    /// The last block.
    last: bool,
    /// The average waveform over the averaged blocks.
    average: bool,
    /// The peak waveform since Reset peaks.
    peak: bool,
}

impl Traces {
    /// As stored: `1,1,0`.
    fn to_setting(self) -> String {
        let bit = |on: bool| if on { "1" } else { "0" };
        format!(
            "{},{},{}",
            bit(self.last),
            bit(self.average),
            bit(self.peak)
        )
    }

    /// From what [`Self::to_setting`] stored; all on for anything else.
    fn from_setting(setting: &str) -> Self {
        match setting.split(',').collect::<Vec<_>>()[..] {
            [last, average, peak] => Self {
                last: last == "1",
                average: average == "1",
                peak: peak == "1",
            },
            _ => Self {
                last: true,
                average: true,
                peak: true,
            },
        }
    }
}

/// The block plot as it was when Hold was pressed.
struct Held {
    last: Vec<f64>,
    average: Vec<f64>,
    peak: Vec<f64>,
    /// The block's span in seconds (its sample rate).
    span: f64,
    /// When it was held, in seconds since start.
    at: f64,
}

struct App {
    started: Instant,
    /// The local clock time at `started`: the history's time axis shows clock time.
    started_wall: DateTime<Local>,
    /// The block plot's dB axis.
    block_y: YScale,
    /// The last export or open: when, what to show, and the file's full path.
    note: Option<(Instant, String, String)>,
    /// The reference line, on or off, and its level.
    reference: bool,
    reference_dbm: f64,
    /// Asking whether to clear the history.
    confirm_clear: bool,
    /// The export file being viewed, if one is (viewing mode: no meter, no sampling).
    file: Option<String>,
    ports: Vec<MeterPort>,
    port_choice: String,
    link: Option<Link>,
    status: Option<String>,
    /// Connected or trying to be (not after Disconnect).
    want_connected: bool,
    auto_reconnect: bool,
    next_attempt: Instant,
    /// When the link last delivered something (or was set up, or the meter set).
    last_data: Instant,
    /// What the meter should be set to.
    wanted: Settings,
    /// The sample rate chosen here (sent with Apply).
    rate: SampleRate,
    /// The sample rate the meter runs at, as far as known: the last one sent to it. The meter
    /// never reports it.
    active_rate: SampleRate,
    /// What the meter last reported.
    reported: Option<Settings>,
    applying: Applying,
    /// Blocks the big number averages over.
    average_frames: usize,
    frames: VecDeque<(Instant, Frame)>,
    peak_dbm: f64,
    /// The block plot's peak waveform since Reset peaks.
    peak_hold: PeakHold,
    /// The block plot frozen with Hold, if it is.
    held: Option<Held>,
    /// Keep the window above all others.
    always_on_top: bool,
    /// When the last blocks came in, for the rate check (since the last setting).
    arrivals: VecDeque<Instant>,
    /// Which traces the block plot shows.
    traces: Traces,
    history: History,
    /// Trigger mode: capture every block whose RMS goes over `trigger_dbm`.
    trigger: bool,
    trigger_dbm: f64,
    /// The blocks the trigger caught, oldest first, kept apart from the history (at most
    /// [`MAX_CAPTURES`]).
    captures: VecDeque<history::Entry>,
    /// The capture Back to live put away (its time): the block plot runs live until a newer one.
    dismissed_capture: Option<f64>,
    /// The capture stepped to with ‹ ›, by its time; `None` follows the newest.
    browsed_capture: Option<f64>,
    /// The capture on screen when Back to live was pressed, by its time: Captures goes back
    /// to it.
    left_capture: Option<f64>,
    view: HistoryView,
    theme: Theme,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        fn parsed<T: std::str::FromStr>(cc: &eframe::CreationContext, key: &str) -> Option<T> {
            cc.storage?.get_string(key)?.parse().ok()
        }
        let stored = |key: &str| cc.storage.and_then(|storage| storage.get_string(key));
        let theme = Theme::from_name(&stored("theme").unwrap_or_default());
        theme::install(&cc.egui_ctx, theme);

        let now = Instant::now();
        let started_wall = Local::now();
        let rate = parsed(cc, "sample_rate")
            .and_then(SampleRate::new)
            .unwrap_or_else(|| SampleRate::new(DEFAULT_RATE).expect("the default rate exists"));
        let mut app = Self {
            started: now,
            started_wall,
            block_y: YScale::Fit,
            note: None,
            file: None,
            confirm_clear: false,
            reference: parsed(cc, "reference").unwrap_or(false),
            reference_dbm: parsed(cc, "reference_dbm").unwrap_or(-30.0),
            ports: Vec::new(),
            port_choice: stored("port").unwrap_or_else(|| AUTO.to_owned()),
            link: None,
            status: None,
            want_connected: true,
            auto_reconnect: parsed(cc, "auto_reconnect").unwrap_or(true),
            next_attempt: now,
            last_data: now,
            wanted: Settings {
                frequency_mhz: parsed(cc, "frequency_mhz").unwrap_or(800),
                attenuation_db: parsed(cc, "attenuation_db").unwrap_or(0.0),
            },
            rate,
            active_rate: rate,
            reported: None,
            applying: Applying::Idle,
            average_frames: parsed(cc, "average_frames").unwrap_or(5),
            frames: VecDeque::new(),
            peak_dbm: f64::NEG_INFINITY,
            peak_hold: PeakHold::new(),
            held: None,
            always_on_top: parsed(cc, "always_on_top").unwrap_or(false),
            arrivals: VecDeque::new(),
            traces: Traces::from_setting(&stored("traces").unwrap_or_default()),
            history: History::default(),
            trigger: parsed(cc, "trigger").unwrap_or(false),
            trigger_dbm: parsed(cc, "trigger_dbm").unwrap_or(-20.0),
            captures: VecDeque::new(),
            dismissed_capture: None,
            browsed_capture: None,
            left_capture: None,
            view: HistoryView::new(parsed(cc, "history_window").unwrap_or(60.0), started_wall),
            theme,
        };
        app.set_always_on_top(&cc.egui_ctx);
        app.refresh_ports();
        app.connect(&cc.egui_ctx);
        app
    }

    /// Puts the window above all others, or back among them, as `always_on_top` says.
    fn set_always_on_top(&self, ctx: &egui::Context) {
        let level = if self.always_on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
    }

    /// Freezes the block plot as it is now (its traces, whichever are shown later).
    fn hold(&mut self) {
        let last = self
            .frames
            .back()
            .map(|(_, frame)| frame.readings.clone())
            .unwrap_or_default();
        let average = rfpm::mean_waveform(self.frames.iter().map(|(_, frame)| frame));
        self.held = Some(Held {
            last,
            average,
            peak: self.peak_hold.peaks().to_vec(),
            span: self.active_rate.block().as_secs_f64(),
            at: self.clock(Instant::now()),
        });
    }

    /// Seconds since the viewer started: the history's time axis.
    fn clock(&self, at: Instant) -> f64 {
        (at - self.started).as_secs_f64()
    }

    fn refresh_ports(&mut self) {
        match rfpm::find_ports() {
            Ok(ports) => self.ports = ports,
            Err(error) => self.status = Some(format!("Can't list ports: {error}")),
        }
    }

    /// Opens the chosen port and sets the meter. Keeps the history; a failure leaves the
    /// reason in the status (and auto-reconnect tries again).
    fn connect(&mut self, ctx: &egui::Context) {
        self.close_file();
        self.link = None;
        self.reported = None;
        self.applying = Applying::Idle;
        let opened = if self.port_choice == AUTO {
            Meter::open_first()
        } else {
            Meter::open(&self.port_choice)
        };
        let meter = match opened {
            Ok(meter) => meter,
            Err(error) => {
                self.status = Some(error.to_string());
                return;
            }
        };
        self.status = None;
        self.frames.clear();
        let link = Link::start(meter, ctx);
        // The meter forgets everything at power-up: set it right away.
        link.apply(ctx, self.wanted, self.rate);
        self.applying = Applying::Sending;
        self.last_data = Instant::now();
        self.link = Some(link);
    }

    fn disconnect(&mut self) {
        self.link = None;
        self.applying = Applying::Idle;
        self.want_connected = false;
        self.status = None;
    }

    /// Reconnects when the link is down or has gone quiet, at most once a second.
    fn keep_connected(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if let Some(link) = &self.link {
            // Each setting pauses the meter for a moment; a slow block takes its span.
            let slowest = self.rate.block().max(self.active_rate.block());
            let quiet_limit = slowest + SETTLE * 3 + STALL_MARGIN;
            if now - self.last_data > quiet_limit {
                self.status = Some(format!("No data from {}", link.name));
                self.link = None;
                self.applying = Applying::Idle;
            }
        }
        if self.link.is_none()
            && self.want_connected
            && self.auto_reconnect
            && now >= self.next_attempt
        {
            self.next_attempt = now + RECONNECT_EVERY;
            let reason = self.status.take();
            self.refresh_ports();
            self.connect(ctx);
            if self.link.is_none() {
                let reason = reason.unwrap_or_else(|| "not connected".to_owned());
                let error = self.status.take().unwrap_or_default();
                // Keep the first reason (the loss), not the same "not found" every second.
                let reason = reason
                    .split(" · reconnecting")
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                // The attempt's error only when it says something the reason doesn't.
                self.status = Some(if error.is_empty() || error == reason {
                    format!("{reason} · reconnecting")
                } else {
                    format!("{reason} · reconnecting ({error})")
                });
            }
        }
    }

    fn apply(&mut self, ctx: &egui::Context) {
        if let Some(link) = &self.link {
            link.apply(ctx, self.wanted, self.rate);
            self.applying = Applying::Sending;
            self.last_data = Instant::now();
            // Other settings: the average and peaks so far don't compare.
            self.frames.clear();
            self.reset_peaks();
        }
    }

    fn reset_peaks(&mut self) {
        self.peak_dbm = f64::NEG_INFINITY;
        self.peak_hold.clear();
    }

    /// Clears the average and the peaks (Reset); the history stays.
    fn reset(&mut self) {
        self.frames.clear();
        self.reset_peaks();
    }

    /// Clears the history and the captures (Clear, after asking).
    fn clear_history(&mut self) {
        self.history.clear();
        self.captures.clear();
        self.browsed_capture = None;
        self.dismissed_capture = None;
        self.left_capture = None;
        self.view.picked = None;
    }

    /// Takes what the threads sent, and asks for the settings again when no reply came.
    fn receive(&mut self) {
        let now = Instant::now();
        if let Some(link) = &self.link {
            let mut lost = None;
            loop {
                match link.messages.try_recv() {
                    Ok(Message::Frame(frame, arrived)) => {
                        self.last_data = now;
                        self.arrivals.push_back(arrived);
                        while self.arrivals.len() > RATE_CHECK_BLOCKS {
                            self.arrivals.pop_front();
                        }
                        self.peak_dbm = self.peak_dbm.max(frame.max_dbm());
                        self.peak_hold.push(&frame);
                        // Timed by when it was sampled, not when it came in over USB.
                        let block = self.active_rate.block();
                        let arrived = (arrived - self.started).as_secs_f64();
                        let middle = history::sampled_middle(arrived, block);
                        // With the settings it was measured with: as the meter reported them,
                        // else as set here; the rate as last sent.
                        let settings = self.reported.unwrap_or(self.wanted);
                        self.history.meta = history::Meta {
                            frequency_mhz: settings.frequency_mhz,
                            attenuation_db: settings.attenuation_db as f32,
                            rate: self.active_rate.code(),
                        };
                        self.history.push(middle, block, &frame);
                        if self.trigger
                            && let Some(entry) = self.history.entries().back()
                        {
                            capture(&mut self.captures, entry, self.trigger_dbm);
                        }
                        self.frames.push_back((now, frame));
                    }
                    Ok(Message::Settings(settings)) => {
                        self.last_data = now;
                        self.reported = Some(settings);
                        self.applying = Applying::Idle;
                    }
                    Ok(Message::RateSet(rate)) => {
                        // Blocks from before the change don't compare with the new ones.
                        self.active_rate = rate;
                        self.arrivals.clear();
                        self.frames.clear();
                        self.peak_dbm = f64::NEG_INFINITY;
                        self.peak_hold.clear();
                    }
                    Ok(Message::Sent) => {
                        // The meter paused for the settings: measure afresh.
                        self.arrivals.clear();
                        self.applying = Applying::Waiting {
                            since: now,
                            retried: false,
                        };
                    }
                    Ok(Message::Error(error)) => {
                        self.status = Some(error);
                        self.applying = Applying::Idle;
                    }
                    Ok(Message::Lost(error)) => {
                        lost = Some(format!("Lost {}: {error}", link.name));
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        lost = Some(format!("Lost {}", link.name));
                        break;
                    }
                }
            }
            // The first `Read` after a setting is sometimes swallowed: ask once more.
            if let Applying::Waiting { since, retried } = self.applying
                && now - since > REPLY_TIMEOUT
            {
                if retried {
                    self.status = Some("The meter doesn't report its settings".to_owned());
                    self.applying = Applying::Idle;
                } else {
                    link.read_settings();
                    self.applying = Applying::Waiting {
                        since: now,
                        retried: true,
                    };
                }
            }
            if let Some(reason) = lost {
                self.status = Some(reason);
                self.link = None;
                self.applying = Applying::Idle;
            }
        }
        while self.frames.len() > self.average_frames.max(1) {
            self.frames.pop_front();
        }
    }

    /// The mean over the averaged blocks (power average), in dBm.
    fn mean_dbm(&self) -> Option<f64> {
        if self.frames.is_empty() {
            return None;
        }
        let mw: f64 = self
            .frames
            .iter()
            .map(|(_, frame)| dbm_to_mw(frame.mean_dbm()))
            .sum();
        Some(10.0 * (mw / self.frames.len() as f64).log10())
    }

    /// The time between blocks (the median over the last ones, so a pause doesn't count) and
    /// the sample rates it fits; `None` until enough blocks came in since the last setting.
    fn rate_check(&self) -> Option<(Duration, Vec<SampleRate>)> {
        if self.arrivals.len() < RATE_CHECK_MIN {
            return None;
        }
        let mut intervals: Vec<Duration> = self
            .arrivals
            .iter()
            .zip(self.arrivals.iter().skip(1))
            .map(|(earlier, later)| *later - *earlier)
            .collect();
        intervals.sort();
        let median = intervals[intervals.len() / 2];
        Some((median, SampleRate::matching(median)))
    }

    /// The attenuation the meter adds, as far as known: what it reported, else what was asked.
    fn attenuation_db(&self) -> f64 {
        self.reported
            .map_or(self.wanted.attenuation_db, |settings| {
                settings.attenuation_db
            })
    }

    /// Row 1: the connection, and the window's own switches at the right.
    fn connection_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let before = self.port_choice.clone();
            egui::ComboBox::from_id_salt("port")
                .selected_text(&self.port_choice)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.port_choice, AUTO.to_owned(), AUTO);
                    for port in &self.ports {
                        let label = match port.interface {
                            Some(interface) => format!("{} (interface {interface})", port.name),
                            None => port.name.clone(),
                        };
                        ui.selectable_value(&mut self.port_choice, port.name.clone(), label);
                    }
                });
            if ui
                .button("Rescan")
                .on_hover_text("Look for meters again")
                .clicked()
            {
                self.refresh_ports();
            }
            // Disconnect only while there is a link; after an unplug it's Connect again (and
            // Auto-reconnect, when on, keeps trying by itself).
            if self.link.is_some() {
                if ui.button("Disconnect").clicked() {
                    self.disconnect();
                }
            } else if ui.button("Connect").clicked() {
                self.want_connected = true;
                self.connect(ui.ctx());
            }
            if self.port_choice != before {
                self.want_connected = true;
                self.connect(ui.ctx());
            }
            theme::toggle(ui, &mut self.auto_reconnect, "Auto-reconnect").on_hover_text(
                "Reconnect by itself when the meter is unplugged or stops sending, and set it \
                 again",
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let before = self.theme;
                egui::ComboBox::from_id_salt("theme")
                    .selected_text(self.theme.name())
                    .show_ui(ui, |ui| {
                        for theme in Theme::ALL {
                            ui.selectable_value(&mut self.theme, theme, theme.name());
                        }
                    })
                    .response
                    .on_hover_text("Colour theme");
                if self.theme != before {
                    theme::set_theme(ui.ctx(), self.theme);
                }
                if theme::toggle(ui, &mut self.always_on_top, "On top")
                    .on_hover_text("Keep this window above all others")
                    .clicked()
                {
                    self.set_always_on_top(ui.ctx());
                }
            });
        });
    }

    /// Row 2: the meter's settings, and what it reports and runs at.
    fn meter_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Frequency");
            ui.add(
                egui::DragValue::new(&mut self.wanted.frequency_mhz)
                    .range(1..=9999)
                    .suffix(" MHz"),
            )
            .on_hover_text("The band you measure: picks the meter's calibration");
            ui.label("Attenuation");
            ui.add(
                egui::DragValue::new(&mut self.wanted.attenuation_db)
                    .range(-99.9..=99.9)
                    .speed(0.1)
                    .fixed_decimals(1)
                    .suffix(" dB"),
            )
            .on_hover_text("What sits in front of the meter: the meter adds it to every reading");
            ui.label("Rate");
            egui::ComboBox::from_id_salt("rate")
                .selected_text(format!("K{:02}", self.rate.code()))
                .show_ui(ui, |ui| {
                    for rate in SampleRate::distinct() {
                        ui.selectable_value(&mut self.rate, rate, rate.to_string());
                    }
                })
                .response
                .on_hover_text(format!(
                    "Sample rate {}.\nThe meter samples a block of 500, then spends ~{} ms \
                     sending it without sampling: 'covers' is the share of time it measures. \
                     Bursts shorter than that gap can go unseen.",
                    self.rate,
                    rfpm::BLOCK_TRANSFER.as_millis()
                ));
            let busy = !matches!(self.applying, Applying::Idle);
            if ui
                .add_enabled(self.link.is_some() && !busy, egui::Button::new("Apply"))
                .clicked()
            {
                self.apply(ui.ctx());
            }

            ui.separator();
            let palette = theme::c();
            match (&self.applying, self.reported) {
                (Applying::Sending | Applying::Waiting { .. }, _) => {
                    ui.label(RichText::new("setting the meter…").color(palette.muted));
                }
                (Applying::Idle, Some(settings)) => {
                    let text = format!(
                        "meter: {} MHz, {:+.1} dB",
                        settings.frequency_mhz, settings.attenuation_db
                    );
                    if settings == self.wanted {
                        ui.label(RichText::new(text).color(palette.muted));
                    } else {
                        ui.label(RichText::new(text).color(palette.echo))
                            .on_hover_text("Not what is set here: Apply");
                    }
                }
                (Applying::Idle, None) => {
                    ui.label(RichText::new("meter: settings unknown").color(palette.muted));
                }
            }
            match self.rate_check() {
                Some((interval, rates)) => {
                    ui.label(
                        RichText::new(format!("· runs at {}", describe_rates(&rates)))
                            .color(palette.muted),
                    )
                    .on_hover_text(format!(
                        "A block every {} ms ({:.1} a second). The meter never reports its \
                         sample rate: this is the rate that time fits.",
                        interval.as_millis(),
                        1.0 / interval.as_secs_f64()
                    ));
                }
                None if self.link.is_some() => {
                    ui.label(RichText::new("· measuring the rate…").color(palette.faint));
                }
                None => {}
            }
        });
    }

    /// The big number, with Average and Reset at the right; a short line on the last block; and
    /// warnings.
    fn readout(&mut self, ui: &mut egui::Ui) {
        let palette = theme::c();
        let mean = self.mean_dbm();
        ui.horizontal(|ui| {
            match mean {
                Some(dbm) => {
                    ui.label(
                        RichText::new(format!("{dbm:.1} dBm")).font(theme::semibold_font(60.0)),
                    );
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(format_power(dbm))
                                .font(theme::semibold_font(24.0))
                                .color(palette.muted),
                        );
                        ui.label(
                            RichText::new(format!("peak {:.1} dBm", self.peak_dbm))
                                .color(palette.muted),
                        );
                    });
                }
                None => {
                    let text = match (&self.link, &self.status) {
                        (_, Some(status)) => status.clone(),
                        (Some(link), None) => format!("Waiting for {}…", link.name),
                        (None, None) => "Not connected".to_owned(),
                    };
                    ui.label(RichText::new(text).size(20.0));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Reset")
                    .on_hover_text("Clear the average and the peaks (Clear in the history bar clears the history)")
                    .clicked()
                {
                    self.reset();
                }
                let unit = if self.average_frames == 1 {
                    " block"
                } else {
                    " blocks"
                };
                ui.add(
                    egui::DragValue::new(&mut self.average_frames)
                        .range(1..=100)
                        .suffix(unit),
                )
                .on_hover_text("Blocks the big number averages over");
                ui.label("Average");
                ui.separator();
                ui.add_enabled(
                    self.reference,
                    egui::DragValue::new(&mut self.reference_dbm)
                        .range(-70.0..=30.0)
                        .speed(0.1)
                        .fixed_decimals(1)
                        .suffix(" dBm"),
                )
                .on_hover_text("The reference level");
                theme::toggle(ui, &mut self.reference, "Ref").on_hover_text(
                    "A reference level, a solid line in both plots: something to compare with, \
                     such as a limit. It doesn't capture anything (that's the trigger).",
                );
            });
        });

        let Some(dbm) = mean else { return };
        let Some((_, last)) = self.frames.back() else {
            return;
        };
        let mut details = format!(
            "last block: min {:.1}, max {:.1} dBm",
            last.min_dbm(),
            last.max_dbm()
        );
        if let Some(link) = &self.link {
            details += &format!(" · {}", link.name);
        }
        if let Some(status) = &self.status {
            details += &format!(" · {status}");
        }
        ui.label(RichText::new(details).color(palette.muted));

        // The meter adds the attenuation: compare its own input with its limits.
        let input = |dbm: f64| dbm - self.attenuation_db();
        if last.readings.iter().all(|&dbm| dbm == CORRUPT_DBM) {
            ui.colored_label(
                palette.alarm,
                "-99.9 dBm: the meter's settings are corrupt. Apply sets them right.",
            );
        } else if input(last.max_dbm()) >= MAX_DBM {
            ui.colored_label(
                palette.alarm,
                format!(
                    "Overload: the meter takes 0 dBm at most and breaks above +{DAMAGE_DBM:.0} \
                     dBm. Add attenuation."
                ),
            );
        } else if input(dbm) <= FLOOR_DBM + FLOOR_MARGIN_DB {
            ui.colored_label(
                palette.echo,
                "At the noise floor: no signal, or below -60 dBm.",
            );
        }
        if let Some((_, rates)) = self.rate_check()
            && matches!(self.applying, Applying::Idle)
            && !rates.is_empty()
            && !rates
                .iter()
                .any(|rate| rate.period() == self.active_rate.period())
        {
            ui.colored_label(
                palette.echo,
                format!(
                    "The meter seems to run at {}, not K{:02} as sent. Apply sets it again.",
                    describe_rates(&rates),
                    self.active_rate.code()
                ),
            );
        }
    }

    /// The block bar: title and what's shown, the trace toggles, and Hold and Reset peaks at the
    /// right (Back to live for a block picked in the history).
    fn block_bar(&mut self, ui: &mut egui::Ui) {
        let palette = theme::c();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Block").font(theme::semibold_font(17.0)));
            let picked = self.view.picked;
            match (picked, &self.held) {
                (Some(at), _) => {
                    ui.label(
                        RichText::new(format!("{} from the history", self.wall(at)))
                            .color(palette.echo),
                    );
                }
                (None, Some(held)) => {
                    ui.label(
                        RichText::new(format!("held at {}", self.wall(held.at)))
                            .color(palette.echo),
                    );
                }
                (None, None) if let Some((index, capture)) = self.shown_capture_at() => {
                    ui.label(
                        RichText::new(format!(
                            "triggered at {} · {} / {}",
                            self.wall(capture.at),
                            index + 1,
                            self.captures.len()
                        ))
                        .color(palette.alarm),
                    );
                }
                (None, None) => {
                    ui.label(RichText::new(rate_summary(self.active_rate)).color(palette.muted));
                }
            }
            if let Some(levels) = self.shown_levels() {
                ui.label(
                    RichText::new(format!("rms {:.1} · avg {:.1} dBm", levels.rms, levels.avg))
                        .color(palette.muted),
                )
                .on_hover_text(
                    "The block's RMS (its power average) and the plain average of its dBm \
                     readings; dashed and dotted in the plot",
                );
            }
            if picked.is_none() {
                ui.add_space(8.0);
                theme::trace_toggle(ui, &mut self.traces.last, "Last", palette.muted);
                let average = format!("Average ({})", self.frames.len());
                theme::trace_toggle(ui, &mut self.traces.average, &average, palette.accent);
                theme::trace_toggle(ui, &mut self.traces.peak, "Peak", palette.echo);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if picked.is_some() {
                    if ui.button("Back to live").clicked() {
                        self.view.picked = None;
                    }
                    return;
                }
                if self.held.is_none()
                    && let Some((index, _)) = self.shown_capture_at()
                {
                    if ui
                        .button("Back to live")
                        .on_hover_text("Run live again until the trigger catches the next block")
                        .clicked()
                    {
                        self.left_capture = self.shown_capture().map(|capture| capture.at);
                        self.browsed_capture = None;
                        self.dismissed_capture = self.captures.back().map(|capture| capture.at);
                    }
                    let newest = index + 1 == self.captures.len();
                    if ui
                        .add_enabled(!newest, egui::Button::new("›"))
                        .on_hover_text("Next capture")
                        .clicked()
                    {
                        self.step_capture(1);
                    }
                    if ui
                        .add_enabled(index > 0, egui::Button::new("‹"))
                        .on_hover_text("Previous capture")
                        .clicked()
                    {
                        self.step_capture(-1);
                    }
                    return;
                }
                if ui
                    .button("Reset peaks")
                    .on_hover_text("Clear the peak trace and the peak reading")
                    .clicked()
                {
                    self.reset_peaks();
                }
                let mut holding = self.held.is_some();
                if theme::toggle(ui, &mut holding, "Hold")
                    .on_hover_text("Freeze the block plot as it is now")
                    .clicked()
                {
                    if holding {
                        self.hold();
                    } else {
                        self.held = None;
                    }
                }
                // Back to the trigger view after Back to live: the newest capture, then ‹ ›.
                if self.trigger && self.held.is_none() && !self.captures.is_empty() {
                    let label = format!("Captures ({})", self.captures.len());
                    if ui
                        .button(RichText::new(label).color(palette.alarm))
                        .on_hover_text(
                            "Back to the captured blocks, at the one you were looking at",
                        )
                        .clicked()
                    {
                        self.resume_captures();
                    }
                }
            });
        });
    }

    fn block_plot(&mut self, ui: &mut egui::Ui, height: f32, full: (f64, f64)) {
        let palette = theme::c();
        let picked = self
            .view
            .picked
            .and_then(|at| self.history.entries().iter().find(|entry| entry.at == at));
        let mut traces = Vec::new();
        let span = if let Some(entry) = picked {
            let values = entry.frame().readings;
            traces.push(Trace {
                name: "picked",
                values,
                color: palette.echo,
                width: 1.5,
            });
            entry.span
        } else if self.held.is_none()
            && let Some(capture) = self.shown_capture()
        {
            let values = capture.frame().readings;
            traces.push(Trace {
                name: "triggered",
                values,
                color: palette.alarm,
                width: 1.5,
            });
            capture.span
        } else if let Some(held) = &self.held {
            let shown = [
                (self.traces.last, "last", &held.last, palette.muted, 1.0),
                (
                    self.traces.average,
                    "average",
                    &held.average,
                    palette.accent,
                    2.0,
                ),
                (self.traces.peak, "peak", &held.peak, palette.echo, 1.5),
            ];
            for (on, name, values, color, width) in shown {
                if on {
                    traces.push(Trace {
                        name,
                        values: values.clone(),
                        color,
                        width,
                    });
                }
            }
            held.span
        } else {
            if self.traces.last
                && let Some((_, frame)) = self.frames.back()
            {
                let values = frame.readings.clone();
                traces.push(Trace {
                    name: "last",
                    values,
                    color: palette.muted,
                    width: 1.0,
                });
            }
            if self.traces.average {
                let values = rfpm::mean_waveform(self.frames.iter().map(|(_, frame)| frame));
                traces.push(Trace {
                    name: "average",
                    values,
                    color: palette.accent,
                    width: 2.0,
                });
            }
            if self.traces.peak {
                let values = self.peak_hold.peaks().to_vec();
                traces.push(Trace {
                    name: "peak",
                    values,
                    color: palette.echo,
                    width: 1.5,
                });
            }
            self.active_rate.block().as_secs_f64()
        };
        let period_ms = span * 1000.0 / rfpm::READINGS_PER_FRAME as f64;
        let levels = self.shown_levels();
        let range = self.db_range(full);
        plots::block(
            ui,
            height,
            traces,
            period_ms,
            &mut self.block_y,
            range,
            levels,
        );
    }

    /// The history bar: title, Live and the window, and what's in view at the right.
    fn history_bar(&mut self, ui: &mut egui::Ui, now: f64, stats: Option<history::Stats>) {
        let palette = theme::c();
        ui.horizontal(|ui| {
            ui.label(RichText::new("History").font(theme::semibold_font(17.0)));
            if let Some(file) = &self.file {
                ui.label(RichText::new(file).color(palette.echo))
                    .on_hover_text(
                        "Viewing an export: no meter, no sampling. Connect goes back to live.",
                    );
            }
            ui.add_space(8.0);
            // Viewing a file there's no newest block, nor any coming in: no Live, no trigger.
            let sampling = self.file.is_none();
            if !sampling {
                self.view.live = false;
            }
            ui.add_enabled_ui(sampling, |ui| {
                let mut live = self.view.live;
                if theme::toggle(ui, &mut live, "Live")
                    .on_hover_text("Follow the newest block")
                    .on_disabled_hover_text("Viewing a file: nothing new comes in")
                    .clicked()
                {
                    if live {
                        self.view.go_live();
                    } else {
                        self.view.live = false;
                        self.view.end = now;
                    }
                }
            });
            egui::ComboBox::from_id_salt("window")
                .selected_text(plots::format_span(self.view.window))
                .show_ui(ui, |ui| {
                    for (seconds, name) in WINDOWS {
                        if ui
                            .selectable_label(self.view.window == seconds, name)
                            .clicked()
                        {
                            self.view.set_window(seconds);
                        }
                    }
                })
                .response
                .on_hover_text(
                    "How much the plot shows. Scroll to zoom the time axis, Ctrl+scroll the dB \
                     axis, drag to look back, click a block to see it above, double-click to \
                     switch the dB axis between fit and full",
                );

            ui.separator();
            let toggled = theme::toggle(ui, &mut self.trigger, "Trigger")
                .on_hover_text(
                    "Capture every block whose RMS goes over the threshold: marked in the \
                     history, kept apart from it (the last 1000), the last one shown in the block \
                     plot. Switching it on or changing the threshold also picks them from the \
                     history already there (a file's too).",
                )
                .changed();
            ui.label("rms >");
            let threshold = ui.add(
                egui::DragValue::new(&mut self.trigger_dbm)
                    .range(-70.0..=30.0)
                    .speed(0.1)
                    .fixed_decimals(1)
                    .suffix(" dBm"),
            );
            // Rescanning copies the blocks it keeps: once a drag ends, not on every step.
            let moved = threshold.drag_stopped() || (threshold.changed() && !threshold.dragged());
            if (toggled || moved) && self.trigger {
                self.rescan_captures();
            }
            if !self.captures.is_empty() {
                ui.label(
                    RichText::new(format!("{} captured", self.captures.len())).color(palette.alarm),
                );
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Right to left: Open, Export, Clear.
                ui.menu_button("Open", |ui| {
                    let files = export_files();
                    if files.is_empty() {
                        ui.label(RichText::new("No exports yet").color(palette.muted));
                    }
                    for path in files {
                        let name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        if ui.button(name).clicked() {
                            self.open(&path);
                            ui.close();
                        }
                    }
                    ui.separator();
                    ui.label(RichText::new("or drop an export on the window").color(palette.muted));
                })
                .response
                .on_hover_text(
                    "Look at an export again: viewing mode, no sampling; Connect goes back to live",
                );
                ui.menu_button("Export", |ui| {
                    let in_view = stats.map_or(0, |stats| stats.blocks);
                    if ui
                        .add_enabled(
                            in_view > 0,
                            egui::Button::new(format!("Blocks in view ({in_view})")),
                        )
                        .clicked()
                    {
                        self.export(now, Export::View);
                        ui.close();
                    }
                    let captured = self.captures.len();
                    if ui
                        .add_enabled(
                            captured > 0,
                            egui::Button::new(format!("Captures ({captured})")),
                        )
                        .clicked()
                    {
                        self.export(now, Export::Captures);
                        ui.close();
                    }
                    let all = self.history.entries().len();
                    if ui
                        .add_enabled(all > 0, egui::Button::new(format!("All ({all})")))
                        .on_hover_text("The whole history, up to 30 minutes")
                        .clicked()
                    {
                        self.export(now, Export::All);
                        ui.close();
                    }
                })
                .response
                .on_hover_text(
                    "Save as CSV to the exports folder next to the program: one row per block \
                     (time, frequency, attenuation, rate, span, peak, rms, avg, min, then its 500 \
                     readings)",
                );
                if ui
                    .add_enabled(
                        !self.history.entries().is_empty(),
                        egui::Button::new("Clear"),
                    )
                    .on_hover_text("Clear the history and the captures (asks first)")
                    .clicked()
                {
                    self.confirm_clear = true;
                }
                if let Some((at, note, path)) = &self.note
                    && at.elapsed() < NOTE_FOR
                {
                    ui.label(RichText::new(note).color(palette.accent))
                        .on_hover_text(path);
                    return;
                }
                let summary = match stats {
                    Some(stats) => format!(
                        "in view: {} blocks · mean {:.1} · max {:.1} · min {:.1} dBm",
                        stats.blocks, stats.mean, stats.max, stats.min
                    ),
                    None => "in view: nothing".to_owned(),
                };
                ui.label(RichText::new(summary).color(palette.muted));
            });
        });
    }

    /// Saves the blocks in view, or the captures, as CSV in the exports folder, and notes where.
    fn export(&mut self, now: f64, what: Export) {
        let (from, to) = self.view.range(now);
        let entries: Vec<_> = match what {
            Export::View => self.history.between(from, to).collect(),
            Export::Captures => self.captures.iter().collect(),
            Export::All => self.history.entries().iter().collect(),
        };
        let csv = history::to_csv(entries.iter().copied(), self.started_wall);
        let name = format!("rfpm-{}.csv", Local::now().format("%Y%m%d-%H%M%S"));
        let folder = exports_folder();
        let saved =
            std::fs::create_dir_all(&folder).and_then(|()| std::fs::write(folder.join(&name), csv));
        let (note, path) = match saved {
            Ok(()) => (
                format!("saved exports/{name} ({} blocks)", entries.len()),
                folder.join(&name).display().to_string(),
            ),
            Err(error) => (
                format!("export failed: {error}"),
                folder.display().to_string(),
            ),
        };
        self.note = Some((Instant::now(), note, path));
    }

    /// Opens an export to look at: disconnects (viewing mode: no sampling) and puts its blocks
    /// in the history, scrolled to them. Connect goes back to live.
    fn open(&mut self, path: &std::path::Path) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let read = std::fs::read_to_string(path)
            .map_err(|error| error.to_string())
            .and_then(|text| history::from_csv(&text, self.started_wall));
        let entries = match read {
            Ok(entries) if !entries.is_empty() => entries,
            Ok(_) => {
                self.note = Some((Instant::now(), format!("{name}: no blocks"), String::new()));
                return;
            }
            Err(error) => {
                let path = path.display().to_string();
                self.note = Some((Instant::now(), format!("can't open {name}: {error}"), path));
                return;
            }
        };
        self.disconnect();
        self.reset();
        self.clear_history();
        self.held = None;
        let from = entries
            .iter()
            .map(history::Entry::start)
            .fold(f64::INFINITY, f64::min);
        let to = entries
            .iter()
            .map(|entry| entry.start() + entry.span)
            .fold(f64::NEG_INFINITY, f64::max);
        let blocks = entries.len();
        self.history.replace(entries);
        if self.trigger {
            self.rescan_captures();
        }
        let margin = ((to - from) * 0.02).max(0.05);
        self.view.live = false;
        self.view.set_window(to - from + 2.0 * margin);
        self.view.end = to + margin;
        self.file = Some(name.clone());
        self.note = Some((
            Instant::now(),
            format!("opened {name} ({blocks} blocks)"),
            path.display().to_string(),
        ));
    }

    /// Leaves viewing mode: forgets the file's blocks.
    fn close_file(&mut self) {
        if self.file.take().is_some() {
            self.reset();
            self.clear_history();
            self.view.go_live();
        }
    }

    /// The capture the block plot shows while the trigger is on, and its index: the one
    /// stepped to with ‹ ›, else the newest (unless Back to live put it away).
    fn shown_capture_at(&self) -> Option<(usize, &history::Entry)> {
        if !self.trigger {
            return None;
        }
        if let Some(at) = self.browsed_capture
            && let Some(index) = self.captures.iter().position(|capture| capture.at == at)
        {
            return Some((index, &self.captures[index]));
        }
        let index = self.captures.len().checked_sub(1)?;
        let newest = &self.captures[index];
        (Some(newest.at) != self.dismissed_capture).then_some((index, newest))
    }

    /// The capture the block plot shows, if any (see [`Self::shown_capture_at`]).
    /// The RMS and plain average of the block the block plot shows: the picked, held, captured
    /// or last one.
    fn shown_levels(&self) -> Option<plots::Levels> {
        let of_entry = |entry: &history::Entry| plots::Levels {
            rms: f64::from(entry.mean),
            avg: f64::from(entry.log_mean),
        };
        if let Some(at) = self.view.picked {
            return self
                .history
                .entries()
                .iter()
                .find(|entry| entry.at == at)
                .map(of_entry);
        }
        if let Some(held) = &self.held {
            return plots::Levels::of(&held.last);
        }
        if let Some(capture) = self.shown_capture() {
            return Some(of_entry(capture));
        }
        plots::Levels::of(&self.frames.back()?.1.readings)
    }

    fn shown_capture(&self) -> Option<&history::Entry> {
        self.shown_capture_at().map(|(_, capture)| capture)
    }

    /// Picks the captures again for the current threshold, from the history already there (see
    /// [`rescan`]); the view follows the newest again.
    fn rescan_captures(&mut self) {
        self.captures = rescan(&self.captures, self.history.entries(), self.trigger_dbm);
        self.browsed_capture = None;
        self.dismissed_capture = None;
        self.left_capture = None;
    }

    /// Back to the captures after Back to live: at the one on screen then, if it's still kept
    /// (the newest follows new ones), else at the newest.
    fn resume_captures(&mut self) {
        let newest = self.captures.back().map(|capture| capture.at);
        let kept = self
            .left_capture
            .filter(|at| self.captures.iter().any(|capture| capture.at == *at));
        self.browsed_capture = kept.filter(|at| Some(*at) != newest);
        self.dismissed_capture = None;
    }

    /// Steps the shown capture by `step` (‹ is -1, › is +1); past the newest it follows new ones.
    fn step_capture(&mut self, step: isize) {
        let Some((index, _)) = self.shown_capture_at() else {
            return;
        };
        let target = index.saturating_add_signed(step);
        if step > 0 && target >= self.captures.len() - 1 {
            self.browsed_capture = None;
        } else {
            self.browsed_capture = self.captures.get(target).map(|capture| capture.at);
        }
        self.dismissed_capture = None;
    }

    /// Asks before clearing the history: it can't be undone.
    fn confirm_clear_dialog(&mut self, ctx: &egui::Context) {
        let palette = theme::c();
        let blocks = self.history.entries().len();
        let captures = self.captures.len();
        let modal = egui::Modal::new(egui::Id::unique("confirm_clear")).show(ctx, |ui| {
            ui.label(RichText::new("Clear the history?").font(theme::semibold_font(20.0)));
            ui.add_space(4.0);
            ui.label(format!(
                "This forgets {blocks} blocks and {captures} captures. It can't be undone; \
                 export first to keep them."
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new("Clear").color(palette.alarm))
                    .clicked()
                {
                    self.clear_history();
                    self.confirm_clear = false;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_clear = false;
                }
            });
        });
        if modal.should_close() {
            self.confirm_clear = false;
        }
    }

    /// Both plots' dB frame: `full` and the reference level, when it's on.
    fn db_range(&self, full: (f64, f64)) -> plots::DbRange {
        plots::DbRange {
            full,
            reference: self.reference.then_some(self.reference_dbm),
        }
    }

    /// Seconds since start as local clock time.
    fn wall(&self, at: f64) -> String {
        plots::wall_clock(self.started_wall, at, 0)
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.confirm_clear {
            self.confirm_clear_dialog(&ctx);
        }
        let dropped = ctx.input(|input| {
            let files = input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf());
            files.collect::<Vec<_>>()
        });
        if let Some(path) = dropped.first() {
            self.open(path);
        }
        self.receive();
        self.keep_connected(&ctx);
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.connection_bar(ui);
            self.meter_bar(ui);
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.readout(ui);
            ui.add_space(8.0);

            let offset = self.attenuation_db();
            let full = (FLOOR_DBM - 5.0 + offset, MAX_DBM + 5.0 + offset);
            // The block plot takes half of what's left after the two bars; the history then
            // takes exactly the rest, so its axis never runs off the window.
            let bars = 2.0 * ui.spacing().interact_size.y + 4.0 * ui.spacing().item_spacing.y + 8.0;
            let block_height = ((ui.available_height() - bars) / 2.0).max(100.0);

            self.block_bar(ui);
            self.block_plot(ui, block_height, full);
            ui.add_space(4.0);

            let now = self.clock(Instant::now());
            let (from, to) = self.view.range(now);
            let stats = self.history.stats(from, to);
            self.history_bar(ui, now, stats);
            let history_height = ui.available_height().max(100.0);
            let range = self.db_range(full);
            plots::history(
                ui,
                history_height,
                &self.history,
                &mut self.view,
                now,
                range,
                self.trigger.then_some(plots::Trigger {
                    threshold: self.trigger_dbm,
                    captures: &self.captures,
                }),
            );
        });
        if self.link.is_some() || self.want_connected {
            // Keep the history scrolling, the settings retry and reconnecting going.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("port", self.port_choice.clone());
        storage.set_string("frequency_mhz", self.wanted.frequency_mhz.to_string());
        storage.set_string("attenuation_db", self.wanted.attenuation_db.to_string());
        storage.set_string("sample_rate", self.rate.code().to_string());
        storage.set_string("average_frames", self.average_frames.to_string());
        storage.set_string("theme", self.theme.name().to_owned());
        storage.set_string("traces", self.traces.to_setting());
        storage.set_string("auto_reconnect", self.auto_reconnect.to_string());
        storage.set_string("history_window", self.view.window.to_string());
        storage.set_string("trigger", self.trigger.to_string());
        storage.set_string("reference", self.reference.to_string());
        storage.set_string("reference_dbm", self.reference_dbm.to_string());
        storage.set_string("trigger_dbm", self.trigger_dbm.to_string());
        storage.set_string("always_on_top", self.always_on_top.to_string());
    }
}

/// What Export saves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Export {
    /// The blocks in view.
    View,
    /// The trigger's captures.
    Captures,
    /// The whole history.
    All,
}

/// The exports folder: `exports` next to the program.
fn exports_folder() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("exports")))
        .unwrap_or_else(|| "exports".into())
}

/// The CSV files in the exports folder, newest first (at most 20).
fn export_files() -> Vec<std::path::PathBuf> {
    let Ok(dir) = std::fs::read_dir(exports_folder()) else {
        return Vec::new();
    };
    let mut files: Vec<_> = dir
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("csv"))
        })
        .collect();
    files.sort();
    files.reverse();
    files.truncate(20);
    files
}

/// Whether `entry` triggers at `threshold`: its RMS is over it, and it's a real reading.
fn triggers(entry: &history::Entry, threshold: f64) -> bool {
    // ±99.9 dBm is the meter saying it has no valid reading, not a level.
    let invalid = |dbm: f32| f64::from(dbm.abs()) >= INVALID_DBM;
    f64::from(entry.mean) > threshold && !(invalid(entry.min) && invalid(entry.max))
}

/// The captures for a new `threshold`, after the fact: those from before `history` still over
/// it, then every block in `history` over it, at most [`MAX_CAPTURES`] (the newest).
fn rescan(
    captures: &VecDeque<history::Entry>,
    history: &VecDeque<history::Entry>,
    threshold: f64,
) -> VecDeque<history::Entry> {
    let history_from = history.front().map_or(f64::INFINITY, |entry| entry.at);
    let older = captures.iter().filter(|capture| capture.at < history_from);
    let mut rescanned: VecDeque<history::Entry> = older
        .chain(history)
        .filter(|entry| triggers(entry, threshold))
        .cloned()
        .collect();
    while rescanned.len() > MAX_CAPTURES {
        rescanned.pop_front();
    }
    rescanned
}

/// Keeps `entry` in `captures` when it [`triggers`] at `threshold`, dropping the oldest beyond
/// [`MAX_CAPTURES`]. Returns whether it was kept.
fn capture(
    captures: &mut VecDeque<history::Entry>,
    entry: &history::Entry,
    threshold: f64,
) -> bool {
    if !triggers(entry, threshold) {
        return false;
    }
    captures.push_back(entry.clone());
    while captures.len() > MAX_CAPTURES {
        captures.pop_front();
    }
    true
}

/// The rates a rate check found: `K05`, `K01–K04`.
fn describe_rates(rates: &[SampleRate]) -> String {
    match rates {
        [] => "no known rate".to_owned(),
        [rate] => format!("K{:02}", rate.code()),
        [first, .., last] => format!("K{:02}–K{:02}", first.code(), last.code()),
    }
}

/// A sample rate in short, for the block bar: `K14 · 2.0 s · 93 %`.
fn rate_summary(rate: SampleRate) -> String {
    let block = rate.block();
    let block = if block < Duration::from_secs(1) {
        format!("{} ms", block.as_millis())
    } else {
        format!("{:.1} s", block.as_secs_f64())
    };
    let coverage = rate.coverage() * 100.0;
    let coverage = if coverage < 10.0 {
        format!("{coverage:.1} %")
    } else {
        format!("{coverage:.0} %")
    };
    format!("K{:02} · {block} · covers {coverage}", rate.code())
}

/// Power in the most readable unit: W, mW, µW, nW or pW.
fn format_power(dbm: f64) -> String {
    let mw = dbm_to_mw(dbm);
    let (value, unit) = match mw {
        mw if mw >= 1000.0 => (mw / 1000.0, "W"),
        mw if mw >= 1.0 => (mw, "mW"),
        mw if mw >= 1e-3 => (mw * 1e3, "µW"),
        mw if mw >= 1e-6 => (mw * 1e6, "nW"),
        mw => (mw * 1e9, "pW"),
    };
    format!("{value:.3} {unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_whole() {
        let icon = icon();
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
    }

    #[test]
    fn traces_round_trip() {
        let traces = Traces {
            last: false,
            average: true,
            peak: false,
        };
        assert_eq!(Traces::from_setting(&traces.to_setting()), traces);
        assert_eq!(
            Traces::from_setting(""),
            Traces {
                last: true,
                average: true,
                peak: true
            }
        );
    }

    #[test]
    fn captures_over_the_threshold() {
        let mut history = History::default();
        for (at, dbm) in [(1.0, -30.0), (2.0, -20.0), (3.0, -10.0)] {
            history.push(
                at,
                Duration::from_millis(2),
                &Frame {
                    readings: vec![dbm; 4],
                },
            );
        }
        let mut captures = VecDeque::new();
        let caught: Vec<bool> = history
            .entries()
            .iter()
            .map(|entry| capture(&mut captures, entry, -20.0))
            .collect();
        // Over, not at: -20 dBm doesn't trigger at -20.
        assert_eq!(caught, [false, false, true]);
        assert_eq!(captures.len(), 1);
        let loud = &history.entries()[2];
        for _ in 0..MAX_CAPTURES + 5 {
            capture(&mut captures, loud, -20.0);
        }
        assert_eq!(captures.len(), MAX_CAPTURES);
        // The meter's +99.9 and -99.9 dBm (no valid reading) never trigger.
        let mut invalid = History::default();
        invalid.push(
            9.0,
            Duration::from_millis(2),
            &Frame {
                readings: vec![99.9; 4],
            },
        );
        invalid.push(
            9.5,
            Duration::from_millis(2),
            &Frame {
                readings: vec![-99.9, 99.9],
            },
        );
        let mut none = VecDeque::new();
        assert!(
            invalid
                .entries()
                .iter()
                .all(|entry| !capture(&mut none, entry, -20.0))
        );
    }

    #[test]
    fn captures_after_the_fact() {
        let mut history = History::default();
        for (at, dbm) in [(1.0, -40.0), (2.0, -25.0), (3.0, -15.0), (4.0, -5.0)] {
            history.push(
                at,
                Duration::from_millis(2),
                &Frame {
                    readings: vec![dbm; 4],
                },
            );
        }
        let at = |captures: &VecDeque<history::Entry>| -> Vec<f64> {
            captures.iter().map(|entry| entry.at).collect()
        };
        // An old capture from before the history, and one now in it.
        let mut older = history.entries()[3].clone();
        older.at = 0.5;
        let captures: VecDeque<_> = [older, history.entries()[3].clone()].into();
        let rescanned = rescan(&captures, history.entries(), -20.0);
        assert_eq!(at(&rescanned), [0.5, 3.0, 4.0]);
        // A higher level drops what's under it, the older one too when under.
        assert_eq!(at(&rescan(&captures, history.entries(), -10.0)), [0.5, 4.0]);
        assert!(rescan(&captures, history.entries(), 0.0).is_empty());
    }

    #[test]
    fn rates_described() {
        let rates = |codes: &[u8]| -> Vec<SampleRate> {
            codes
                .iter()
                .map(|&code| SampleRate::new(code).unwrap())
                .collect()
        };
        assert_eq!(describe_rates(&[]), "no known rate");
        assert_eq!(describe_rates(&rates(&[7])), "K07");
        assert_eq!(describe_rates(&rates(&[1, 2, 3, 4])), "K01–K04");
    }

    #[test]
    fn rate_in_short() {
        assert_eq!(
            rate_summary(SampleRate::new(14).unwrap()),
            "K14 · 2.0 s · covers 93 %"
        );
        assert_eq!(
            rate_summary(SampleRate::new(5).unwrap()),
            "K05 · 16 ms · covers 9.8 %"
        );
    }

    #[test]
    fn power_in_readable_units() {
        assert_eq!(format_power(0.0), "1.000 mW");
        assert_eq!(format_power(23.0), "199.526 mW");
        assert_eq!(format_power(-30.0), "1.000 µW");
        assert_eq!(format_power(-60.0), "1.000 nW");
        assert_eq!(format_power(-70.0), "100.000 pW");
    }
}
