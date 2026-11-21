//! The two plots, on egui_plot: the block (500 samples) and the scrollable history.
//!
//! Both behave the same: the wheel zooms the time axis, Ctrl+wheel the dB axis, dragging pans
//! the time axis, and a double click switches the dB axis between fitting the data and the
//! meter's full range. egui_plot only draws: the ranges are set here every frame.

use chrono::{DateTime, Local, TimeDelta};
use egui::{Color32, PointerButton, Stroke, Vec2, Vec2b};
use egui_plot::{FilledArea, HoverPosition, Line, Plot, PlotPoints, PlotUi, VLine};

use crate::history::{History, KEEP};
use crate::theme;

/// One line in the block plot.
pub struct Trace {
    pub name: &'static str,
    pub values: Vec<f64>,
    pub color: Color32,
    pub width: f32,
}

/// How strongly one notch of the wheel zooms.
const WHEEL_ZOOM: f32 = 0.0025;

/// Room above and below the data when fitting, in dB, and the least span shown.
const FIT_MARGIN_DB: f64 = 2.0;
const FIT_MIN_SPAN_DB: f64 = 6.0;

/// The zoom factor the mouse wheel asks for over this plot (above 1 zooms in), if any.
fn wheel_zoom(plot: &PlotUi) -> Option<f32> {
    if !plot.response().hovered() {
        return None;
    }
    let scroll = plot.ctx().input(|input| input.smooth_scroll_delta.y);
    (scroll != 0.0).then(|| (scroll * WHEEL_ZOOM).exp())
}

/// The zoom factor Ctrl+wheel (or a pinch) asks for over this plot (above 1 zooms in), if any.
fn ctrl_zoom(plot: &PlotUi) -> Option<f32> {
    if !plot.response().hovered() {
        return None;
    }
    let zoom = plot.ctx().input(|input| input.zoom_delta());
    (zoom != 1.0).then_some(zoom)
}

/// How a plot's dB axis is scaled.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum YScale {
    /// Around the data shown, with a little room.
    #[default]
    Fit,
    /// The meter's full range.
    Full,
    /// Zoomed by hand (Ctrl+wheel).
    Zoomed(f64, f64),
}

impl YScale {
    /// The dB range to show for data between `data` (lowest, highest) and the `full` range.
    pub fn range(self, data: Option<(f64, f64)>, full: (f64, f64)) -> (f64, f64) {
        match (self, data) {
            (Self::Zoomed(low, high), _) => (low, high),
            (Self::Fit, Some((low, high))) if low.is_finite() && high.is_finite() => {
                let (low, high) = (low - FIT_MARGIN_DB, high + FIT_MARGIN_DB);
                let missing = (FIT_MIN_SPAN_DB - (high - low)).max(0.0) / 2.0;
                (low - missing, high + missing)
            }
            _ => full,
        }
    }

    /// What a double click makes of it: fit and full swap, a zoom goes back to fit.
    fn toggled(self) -> Self {
        match self {
            Self::Fit => Self::Full,
            Self::Full | Self::Zoomed(..) => Self::Fit,
        }
    }

    /// `shown` zoomed by `factor` (above 1 zooms in) around `centre`.
    fn zoomed(shown: (f64, f64), centre: f64, factor: f64) -> Self {
        let centre = centre.clamp(shown.0, shown.1);
        Self::Zoomed(
            centre - (centre - shown.0) / factor,
            centre + (shown.1 - centre) / factor,
        )
    }
}

/// Applies Ctrl+wheel and double clicks to `scale`, then sets the plot's dB axis.
fn drive_y(plot: &mut PlotUi, scale: &mut YScale, data: Option<(f64, f64)>, full: (f64, f64)) {
    let shown = scale.range(data, full);
    if let Some(factor) = ctrl_zoom(plot) {
        let centre = plot
            .pointer_coordinate()
            .map_or((shown.0 + shown.1) / 2.0, |p| p.y);
        *scale = YScale::zoomed(shown, centre, f64::from(factor));
    }
    if plot.response().double_clicked() {
        *scale = scale.toggled();
    }
    let (low, high) = scale.range(data, full);
    plot.set_plot_bounds_y(low..=high);
}

/// The lowest and highest of `values` (finite ones), if any.
fn extent<'a>(values: impl IntoIterator<Item = &'a f64>) -> Option<(f64, f64)> {
    values
        .into_iter()
        .filter(|value| value.is_finite())
        .fold(None, |extent, &value| {
            Some(extent.map_or((value, value), |(low, high): (f64, f64)| {
                (low.min(value), high.max(value))
            }))
        })
}

/// Plots `traces` over the block, x in ms from the block's first sample (`period_ms` apart),
/// the dB axis as `y` says (`full`: the meter's range).
pub fn block(
    ui: &mut egui::Ui,
    height: f32,
    traces: Vec<Trace>,
    period_ms: f64,
    y: &mut YScale,
    full: (f64, f64),
) {
    let span_ms = period_ms * (rfpm::READINGS_PER_FRAME as f64 - 1.0);
    let data = extent(traces.iter().flat_map(|trace| &trace.values));
    let response = Plot::new("block")
        .height(height)
        .show_background(false)
        .default_x_bounds(0.0, span_ms)
        .allow_zoom(false)
        .allow_drag(Vec2b::new(true, false))
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .grid_color(theme::c().rule)
        .y_axis_min_width(36.0)
        .x_axis_formatter(|mark, _| format_ms(mark.value))
        .y_axis_formatter(|mark, _| format!("{:.0}", mark.value))
        // Its own tooltip (below) shows every trace at the hovered sample.
        .label_formatter(|_| None)
        .show(ui, |plot| {
            if let Some(factor) = wheel_zoom(plot) {
                plot.zoom_bounds_around_hovered(Vec2::new(factor, 1.0));
            }
            drive_y(plot, y, data, full);
            let pointer = plot.pointer_coordinate();
            for trace in &traces {
                let points = trace
                    .values
                    .iter()
                    .enumerate()
                    .map(|(i, &dbm)| [i as f64 * period_ms, dbm])
                    .collect::<Vec<_>>();
                plot.line(
                    Line::new(trace.name, PlotPoints::new(points))
                        .color(trace.color)
                        .width(trace.width),
                );
            }
            pointer
        });

    if response.response.hovered()
        && let Some(pointer) = response.inner
        && let Some(index) = sample_at(pointer.x, period_ms)
    {
        let rows: Vec<(&str, Color32, f64)> = traces
            .iter()
            .filter_map(|trace| Some((trace.name, trace.color, *trace.values.get(index)?)))
            .collect();
        if !rows.is_empty() {
            let time = format_ms(index as f64 * period_ms);
            response
                .response
                .on_hover_ui_at_pointer(|ui| value_tooltip(ui, &time, &rows));
        }
    }
}

/// The sample index nearest to `x` ms, when it's within the block.
fn sample_at(x: f64, period_ms: f64) -> Option<usize> {
    let index = (x / period_ms).round();
    (0.0..rfpm::READINGS_PER_FRAME as f64)
        .contains(&index)
        .then_some(index as usize)
}

/// The narrowest history window, in seconds.
const MIN_WINDOW: f64 = 0.5;

/// What the history plot looks at.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryView {
    /// Following the newest block (the window slides along).
    pub live: bool,
    /// The window's width in seconds.
    pub window: f64,
    /// The window's end when not live, in seconds since start.
    pub end: f64,
    /// The block picked with a click (its arrival time), shown in the block plot.
    pub picked: Option<f64>,
    /// The dB axis.
    pub y: YScale,
}

impl HistoryView {
    /// A live view `window` seconds wide.
    pub fn new(window: f64) -> Self {
        Self {
            live: true,
            window,
            end: 0.0,
            picked: None,
            y: YScale::Fit,
        }
    }

    /// The x range shown at `now`.
    pub fn range(&self, now: f64) -> (f64, f64) {
        let end = if self.live { now } else { self.end };
        (end - self.window, end)
    }

    /// A new window width, keeping the window's end.
    pub fn set_window(&mut self, window: f64) {
        self.window = window.clamp(MIN_WINDOW, KEEP.as_secs_f64());
    }

    /// Zooms the time axis by `factor` (above 1 zooms in): live, the window narrows and keeps
    /// following; otherwise around `pointer` (seconds since start), so the time under it stays.
    pub fn zoom(&mut self, factor: f64, pointer: Option<f64>, now: f64) {
        let (from, to) = self.range(now);
        let window = (self.window / factor).clamp(MIN_WINDOW, KEEP.as_secs_f64());
        if !self.live {
            let anchor = pointer.unwrap_or((from + to) / 2.0).clamp(from, to);
            self.end = anchor + (to - anchor) * window / self.window;
        }
        self.window = window;
    }

    /// Back to following the newest block.
    pub fn go_live(&mut self) {
        self.live = true;
    }
}

/// Plots the history: the band between each block's lowest and highest reading, the block means
/// on it, and the picked block, the time axis in clock time (`start` is second 0). Hovering
/// shows the block's peak, RMS (power average), plain average of the dBm readings and minimum. Dragging
/// looks back (and stops following), the wheel zooms the time axis (live: the window, which
/// keeps following), a click picks a block.
pub fn history(
    ui: &mut egui::Ui,
    height: f32,
    history: &History,
    view: &mut HistoryView,
    now: f64,
    start: DateTime<Local>,
    full: (f64, f64),
) {
    let palette = theme::c();
    let response = Plot::new("history")
        .height(height)
        .show_background(false)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_double_click_reset(false)
        .allow_boxed_zoom(false)
        .grid_color(palette.rule)
        .y_axis_min_width(36.0)
        .x_axis_formatter(move |mark, range| {
            let fine = range.end() - range.start() < 10.0;
            wall_clock(start, mark.value, fine)
        })
        .y_axis_formatter(|mark, _| format!("{:.0}", mark.value))
        .label_formatter(move |hover| {
            let (_, position) = hover_point(hover);
            let time = wall_clock(start, position.x, true);
            // Over a block its own tooltip (below) shows the statistics in columns.
            history
                .at(position.x)
                .is_none()
                .then(|| format!("{time}\n{:.1} dBm", position.y))
        })
        .show(ui, |plot| {
            if plot.response().dragged_by(PointerButton::Primary) {
                let delta = plot.pointer_coordinate_drag_delta().x;
                if delta != 0.0 {
                    let (_, end) = view.range(now);
                    view.live = false;
                    view.end = end - f64::from(delta);
                }
            }
            if let Some(factor) = wheel_zoom(plot) {
                let pointer = plot.pointer_coordinate().map(|pointer| pointer.x);
                view.zoom(f64::from(factor), pointer, now);
            }
            let (from, to) = view.range(now);
            plot.set_plot_bounds_x(from..=to);
            let data = history.stats(from, to).map(|stats| (stats.min, stats.max));
            drive_y(plot, &mut view.y, data, full);

            // Including a block beyond each edge, so lines run off the edges.
            for run in history.runs(from, to) {
                let xs: Vec<f64> = run.iter().map(|entry| entry.at).collect();
                let lows: Vec<f64> = run.iter().map(|entry| f64::from(entry.min)).collect();
                let highs: Vec<f64> = run.iter().map(|entry| f64::from(entry.max)).collect();
                plot.add(
                    FilledArea::new("min–max", &xs, &lows, &highs)
                        .fill_color(palette.accent.gamma_multiply(0.25))
                        .stroke(Stroke::NONE)
                        .allow_hover(false),
                );
                let means: Vec<[f64; 2]> = run
                    .iter()
                    .map(|entry| [entry.at, f64::from(entry.mean)])
                    .collect();
                plot.line(
                    Line::new("mean", PlotPoints::new(means))
                        .color(palette.accent)
                        .width(1.5),
                );
            }
            if let Some(picked) = view.picked {
                plot.vline(VLine::new("picked", picked).color(palette.echo).width(1.5));
            }
            plot.pointer_coordinate()
        });

    if response.response.clicked()
        && let Some(pointer) = response.inner
        && let Some(entry) = history.nearest(pointer.x)
    {
        view.picked = Some(entry.at);
    }
    if response.response.hovered()
        && let Some(pointer) = response.inner
        && let Some(entry) = history.at(pointer.x)
    {
        let time = wall_clock(start, entry.at, true);
        let muted = theme::c().muted;
        let rows = [
            ("peak", muted, f64::from(entry.max)),
            ("rms", muted, f64::from(entry.mean)),
            ("avg", muted, f64::from(entry.log_mean)),
            ("min", muted, f64::from(entry.min)),
        ];
        response
            .response
            .on_hover_ui_at_pointer(|ui| value_tooltip(ui, &time, &rows));
    }
}

/// The width of the values in a tooltip, enough for `-100.0 dBm`.
const TOOLTIP_VALUE_WIDTH: f32 = 76.0;

/// A tooltip of values in dBm: a header, then a row per value with its name (in its colour)
/// left and the value right-aligned.
fn value_tooltip(ui: &mut egui::Ui, header: &str, rows: &[(&str, Color32, f64)]) {
    // Rows as high as the text: the 30 px control height is for buttons.
    let row_height = ui.text_style_height(&egui::TextStyle::Body);
    ui.spacing_mut().interact_size.y = row_height;
    ui.label(egui::RichText::new(header).strong());
    egui::Grid::new("value_tooltip")
        .num_columns(2)
        .spacing([12.0, 0.0])
        .show(ui, |ui| {
            for &(name, color, dbm) in rows {
                ui.label(egui::RichText::new(name).color(color));
                // A fixed width to align in: a right-to-left layout alone takes all there is.
                let size = egui::vec2(TOOLTIP_VALUE_WIDTH, row_height);
                ui.allocate_ui_with_layout(
                    size,
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.label(format!("{dbm:.1} dBm"));
                    },
                );
                ui.end_row();
            }
        });
}

/// The nearest data point's name (with a newline) and position, or just the position.
fn hover_point(hover: &HoverPosition<'_>) -> (String, egui_plot::PlotPoint) {
    match hover {
        HoverPosition::NearDataPoint {
            plot_name,
            position,
            ..
        } => (format!("{plot_name}: "), *position),
        HoverPosition::Elsewhere { position } => (String::new(), *position),
    }
}

/// The local clock time `seconds` after `start`: `22:40:15`, or `22:40:15.3` when `fine`.
pub fn wall_clock(start: DateTime<Local>, seconds: f64, fine: bool) -> String {
    let at = start + TimeDelta::milliseconds((seconds * 1000.0).round() as i64);
    let whole = at.format("%H:%M:%S");
    if fine {
        format!("{whole}.{}", at.timestamp_subsec_millis() / 100)
    } else {
        whole.to_string()
    }
}

/// Milliseconds, in µs below 1 ms.
pub fn format_ms(ms: f64) -> String {
    if ms.abs() < 1.0 && ms != 0.0 {
        format!("{:.0} µs", ms * 1000.0)
    } else if ms.abs() < 1000.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{:.2} s", ms / 1000.0)
    }
}

/// A span of time for a label: `45 s`, `5 min`, `1.5 min`.
pub fn format_span(seconds: f64) -> String {
    if seconds < 60.0 {
        format!("{seconds:.0} s")
    } else {
        let minutes = seconds / 60.0;
        if (minutes - minutes.round()).abs() < 0.05 {
            format!("{minutes:.0} min")
        } else {
            format!("{minutes:.1} min")
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn clock_time() {
        let start = Local.with_ymd_and_hms(2026, 11, 21, 22, 40, 0).unwrap();
        assert_eq!(wall_clock(start, 15.3, false), "22:40:15");
        assert_eq!(wall_clock(start, 15.3, true), "22:40:15.3");
        assert_eq!(wall_clock(start, -60.0, false), "22:39:00");
    }

    #[test]
    fn spans_and_milliseconds() {
        assert_eq!(format_span(45.0), "45 s");
        assert_eq!(format_span(300.0), "5 min");
        assert_eq!(format_span(90.0), "1.5 min");
        assert_eq!(format_ms(0.25), "250 µs");
        assert_eq!(format_ms(128.0), "128 ms");
        assert_eq!(format_ms(8192.0), "8.19 s");
    }

    #[test]
    fn y_scaling() {
        let full = (-65.0, 5.0);
        // Fit: the data with room, at least 6 dB.
        assert_eq!(
            YScale::Fit.range(Some((-61.0, -59.0)), full),
            (-63.0, -57.0)
        );
        assert_eq!(
            YScale::Fit.range(Some((-60.0, -60.0)), full),
            (-63.0, -57.0)
        );
        assert_eq!(
            YScale::Fit.range(Some((-60.0, -20.0)), full),
            (-62.0, -18.0)
        );
        assert_eq!(YScale::Fit.range(None, full), full);
        assert_eq!(YScale::Full.range(Some((-61.0, -59.0)), full), full);
        // A double click swaps fit and full; a zoom goes back to fit.
        assert_eq!(YScale::Fit.toggled(), YScale::Full);
        assert_eq!(YScale::Full.toggled(), YScale::Fit);
        assert_eq!(YScale::Zoomed(-1.0, 1.0).toggled(), YScale::Fit);
        // Zooming keeps the value under the pointer.
        assert_eq!(
            YScale::zoomed((-60.0, -40.0), -50.0, 2.0),
            YScale::Zoomed(-55.0, -45.0)
        );
    }

    #[test]
    fn sample_under_the_pointer() {
        assert_eq!(sample_at(0.2, 1.0), Some(0));
        assert_eq!(sample_at(10.6, 1.0), Some(11));
        assert_eq!(sample_at(499.4, 1.0), Some(499));
        assert_eq!(sample_at(499.6, 1.0), None);
        assert_eq!(sample_at(-0.6, 1.0), None);
    }

    #[test]
    fn data_extent() {
        assert_eq!(
            extent(&[-60.0, f64::NEG_INFINITY, -40.0]),
            Some((-60.0, -40.0))
        );
        assert_eq!(extent(&[]), None);
    }

    #[test]
    fn view_range_and_zoom() {
        let mut view = HistoryView::new(60.0);
        assert_eq!(view.range(100.0), (40.0, 100.0));
        view.zoom(2.0, Some(70.0), 100.0);
        assert_eq!(view.range(100.0), (70.0, 100.0));
        assert!(view.live);
        view.live = false;
        view.end = 50.0; // showing 20 to 50
        view.zoom(2.0, Some(26.0), 100.0);
        assert_eq!(view.range(100.0), (23.0, 38.0));
        view.zoom(0.5, Some(26.0), 100.0);
        assert_eq!(view.range(100.0), (20.0, 50.0));
        view.zoom(1000.0, None, 100.0);
        assert_eq!(view.window, MIN_WINDOW);
        view.go_live();
        assert_eq!(view.range(100.0), (99.5, 100.0));
    }
}
