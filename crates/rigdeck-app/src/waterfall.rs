//! Spectrum and waterfall drawn on the GPU (wgpu, Metal on macOS).
//!
//! The waterfall is one texture used as a ring buffer: each sweep writes one row with a partial
//! texture update (`set_partial`), and drawing splits the texture into two quads so the newest
//! row is at the top. Raw amplitudes are kept as well, so changing the colour map, floor,
//! ceiling or brightness recolours the whole history at once.

use crate::theme::AppTheme;
use eframe::egui::{self, Color32, ColorImage, Mesh, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, pos2, vec2};
use rigdeck_core::rig::{Mode, ModeSetting};
use rigdeck_core::scope::{AMP_MAX, Sweep};
use serde::{Deserialize, Serialize};

pub const HISTORY: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColourMap {
    Classic,
    SmartSdr,
    Turbo,
    Viridis,
    Inferno,
    Grayscale,
    NightRed,
    Ocean,
}

impl ColourMap {
    pub const ALL: [ColourMap; 8] = [
        ColourMap::Classic,
        ColourMap::SmartSdr,
        ColourMap::Turbo,
        ColourMap::Viridis,
        ColourMap::Inferno,
        ColourMap::Grayscale,
        ColourMap::NightRed,
        ColourMap::Ocean,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ColourMap::Classic => "Classic (blue → red)",
            ColourMap::SmartSdr => "Deep blue / green",
            ColourMap::Turbo => "Turbo",
            ColourMap::Viridis => "Viridis",
            ColourMap::Inferno => "Inferno",
            ColourMap::Grayscale => "Grayscale",
            ColourMap::NightRed => "Night red",
            ColourMap::Ocean => "Ocean (teal)",
        }
    }

    fn stops(self) -> &'static [(f32, [u8; 3])] {
        match self {
            ColourMap::Classic => &[
                (0.00, [5, 5, 26]),
                (0.25, [13, 38, 153]),
                (0.45, [0, 178, 217]),
                (0.65, [242, 230, 26]),
                (0.85, [242, 51, 13]),
                (1.00, [255, 255, 255]),
            ],
            ColourMap::SmartSdr => &[
                (0.00, [0, 0, 70]),
                (0.30, [0, 40, 210]),
                (0.55, [0, 200, 90]),
                (0.75, [235, 235, 0]),
                (0.90, [255, 90, 0]),
                (1.00, [255, 255, 255]),
            ],
            ColourMap::Turbo => &[
                (0.00, [48, 18, 59]),
                (0.15, [69, 117, 237]),
                (0.35, [26, 228, 182]),
                (0.55, [164, 252, 60]),
                (0.75, [251, 185, 56]),
                (0.90, [228, 70, 10]),
                (1.00, [122, 4, 3]),
            ],
            ColourMap::Viridis => {
                &[(0.0, [68, 1, 84]), (0.25, [59, 82, 139]), (0.5, [33, 145, 140]), (0.75, [94, 201, 98]), (1.0, [253, 231, 37])]
            }
            ColourMap::Inferno => {
                &[(0.0, [0, 0, 4]), (0.25, [87, 16, 110]), (0.5, [188, 55, 84]), (0.75, [249, 142, 9]), (1.0, [252, 255, 164])]
            }
            ColourMap::Grayscale => &[(0.0, [0, 0, 0]), (1.0, [255, 255, 255])],
            ColourMap::NightRed => {
                &[(0.0, [0, 0, 0]), (0.4, [70, 0, 0]), (0.7, [190, 10, 10]), (0.9, [255, 70, 30]), (1.0, [255, 160, 120])]
            }
            ColourMap::Ocean => {
                &[(0.0, [2, 12, 24]), (0.35, [0, 70, 110]), (0.6, [0, 170, 170]), (0.85, [140, 240, 200]), (1.0, [255, 255, 255])]
            }
        }
    }

    /// Colour for a position 0..1.
    pub fn at(self, t: f32) -> Color32 {
        let stops = self.stops();
        let t = t.clamp(0.0, 1.0);
        let k = stops.iter().position(|s| s.0 >= t).unwrap_or(stops.len() - 1).max(1);
        let (t0, a) = stops[k - 1];
        let (t1, b) = stops[k];
        let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
        let ch = |j: usize| (f32::from(a[j]) + (f32::from(b[j]) - f32::from(a[j])) * f).round() as u8;
        Color32::from_rgb(ch(0), ch(1), ch(2))
    }
}

/// Display settings for the spectrum and waterfall (saved with the app settings).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplaySettings {
    pub colour_map: ColourMap,
    /// Amplitude (0–160) drawn as the darkest colour.
    pub floor: u8,
    /// Amplitude (0–160) drawn as the brightest colour.
    pub ceiling: u8,
    /// Brightness: below 1 lightens, above 1 darkens (a gamma curve over floor..ceiling).
    pub gamma: f32,
    /// Share of the plot height given to the spectrum (the rest is waterfall).
    pub spectrum_share: f32,
    pub fill: bool,
    /// Spectrum averaging, 0 = none, 0.9 = heavy.
    pub smoothing: f32,
    pub show_passband: bool,
    /// A click near a signal tunes to the signal's peak instead of the exact click position.
    pub snap_to_signal: bool,
    /// Clicks tune to the nearest multiple of this (1000 = whole kHz, e.g. 7.212 not 7.2116).
    /// 0 = use the tuning step.
    pub click_round_hz: u64,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            colour_map: ColourMap::Classic,
            floor: 10,
            ceiling: 140,
            gamma: 1.0,
            spectrum_share: 0.38,
            fill: true,
            smoothing: 0.5,
            show_passband: true,
            snap_to_signal: true,
            click_round_hz: 1_000,
        }
    }
}

impl DisplaySettings {
    fn lut(&self) -> [Color32; AMP_MAX as usize + 1] {
        let mut lut = [Color32::BLACK; AMP_MAX as usize + 1];
        let lo = f32::from(self.floor.min(self.ceiling.saturating_sub(1)));
        let hi = f32::from(self.ceiling.max(self.floor + 1));
        for (i, c) in lut.iter_mut().enumerate() {
            let t = ((i as f32 - lo) / (hi - lo)).clamp(0.0, 1.0).powf(self.gamma.clamp(0.2, 4.0));
            *c = self.colour_map.at(t);
        }
        lut
    }
}

/// Approximate receive passband (low, high offsets in Hz from the tuned frequency) for drawing.
/// Widths follow the IC-7300's default FIL1/FIL2/FIL3 settings closely enough for a display aid.
pub fn passband(m: ModeSetting) -> (i64, i64) {
    let f = m.filter.unwrap_or(1).clamp(1, 3) as usize - 1;
    let pick = |w: [i64; 3]| w[f];
    match m.mode {
        Mode::Usb => (100, 100 + pick([3000, 2400, 1800])),
        Mode::Lsb => (-(100 + pick([3000, 2400, 1800])), -100),
        Mode::Cw | Mode::CwR => {
            let w = pick([1200, 500, 250]);
            (-w / 2, w / 2)
        }
        Mode::Rtty | Mode::RttyR => {
            let w = pick([2400, 500, 250]);
            (-w / 2, w / 2)
        }
        Mode::Am | Mode::AmN => {
            let w = pick([9000, 6000, 3000]);
            (-w / 2, w / 2)
        }
        Mode::Fm | Mode::FmN | Mode::C4fm => {
            let w = pick([15000, 10000, 7000]);
            (-w / 2, w / 2)
        }
    }
}

/// Index of the strongest signal within `radius` bins of `at`, if it stands clearly above the
/// noise around it (12 scope units over the median of the window).
pub fn find_peak(spectrum: &[f32], at: usize, radius: usize) -> Option<usize> {
    if spectrum.is_empty() {
        return None;
    }
    let lo = at.saturating_sub(radius);
    let hi = (at + radius).min(spectrum.len() - 1);
    let (best, &val) = spectrum[lo..=hi].iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    let wide_lo = at.saturating_sub(radius * 4);
    let wide_hi = (at + radius * 4).min(spectrum.len() - 1);
    let mut around: Vec<f32> = spectrum[wide_lo..=wide_hi].to_vec();
    around.sort_by(|a, b| a.total_cmp(b));
    let median = around[around.len() / 2];
    (val >= median + 12.0).then_some(lo + best)
}

pub struct Waterfall {
    texture: Option<TextureHandle>,
    width: usize,
    head: usize,
    rows_written: usize,
    raw: Vec<u8>,
    lut: [Color32; AMP_MAX as usize + 1],
    lut_for: Option<DisplaySettings>,
    pub last: Option<Sweep>,
    smooth: Vec<f32>,
}

impl Default for Waterfall {
    fn default() -> Self {
        let d = DisplaySettings::default();
        Self {
            texture: None,
            width: 0,
            head: 0,
            rows_written: 0,
            raw: Vec::new(),
            lut: d.lut(),
            lut_for: None,
            last: None,
            smooth: Vec::new(),
        }
    }
}

/// What the operator did on the plot this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PlotAction {
    pub tune_to: Option<u64>,
    /// Wheel movement over the plot in lines (positive = up / higher frequency), and the
    /// modifier keys held: (shift, alt, command).
    pub scroll: Option<crate::app::Wheel>,
}

impl Waterfall {
    pub fn clear(&mut self) {
        self.texture = None;
        self.width = 0;
        self.rows_written = 0;
        self.raw.clear();
        self.last = None;
        self.smooth.clear();
    }

    /// Frequency a click at `frac` (0 = left edge, 1 = right edge) tunes to: the peak of a nearby
    /// signal when snapping is on and there is one, else the click position; rounded to the step.
    /// `radius_frac` is how far (as a share of the width) to look for a signal.
    pub fn click_frequency(&self, frac: f32, d: &DisplaySettings, step_hz: u64, radius_frac: f32) -> Option<u64> {
        let sweep = self.last.as_ref()?;
        let span = sweep.high_hz.saturating_sub(sweep.low_hz).max(1) as f64;
        let n = self.smooth.len();
        let frac = frac.clamp(0.0, 1.0);
        let mut hz = sweep.low_hz as f64 + span * f64::from(frac);
        if d.snap_to_signal && n >= 2 {
            let i = (frac * (n - 1) as f32).round() as usize;
            let radius = (radius_frac * n as f32).ceil().max(2.0) as usize;
            if let Some(k) = find_peak(&self.smooth, i, radius) {
                hz = sweep.low_hz as f64 + span * k as f64 / (n - 1) as f64;
            }
        }
        let s = if d.click_round_hz > 0 { d.click_round_hz } else { step_hz.max(1) } as f64;
        Some(((hz / s).round() * s) as u64)
    }

    fn recolour_all(&mut self) {
        let Some(t) = &mut self.texture else { return };
        let pixels: Vec<Color32> = self.raw.iter().map(|&p| self.lut[p.min(AMP_MAX) as usize]).collect();
        t.set(ColorImage::new([self.width, HISTORY], pixels), TextureOptions::LINEAR);
    }

    /// Rebuild the colour table when the display settings change, and recolour the history.
    pub fn apply_settings(&mut self, d: &DisplaySettings) {
        if self.lut_for.as_ref() == Some(d) {
            return;
        }
        let colours_changed = self
            .lut_for
            .as_ref()
            .is_none_or(|o| o.colour_map != d.colour_map || o.floor != d.floor || o.ceiling != d.ceiling || o.gamma != d.gamma);
        self.lut_for = Some(d.clone());
        if colours_changed {
            self.lut = d.lut();
            self.recolour_all();
        }
    }

    pub fn push(&mut self, ctx: &egui::Context, sweep: Sweep, smoothing: f32) {
        let w = sweep.pixels.len();
        if w == 0 {
            self.last = Some(sweep);
            return;
        }
        if self.texture.is_none() || self.width != w {
            self.width = w;
            self.raw = vec![0; w * HISTORY];
            let img = ColorImage::filled([w, HISTORY], self.lut[0]);
            self.texture = Some(ctx.load_texture("waterfall", img, TextureOptions::LINEAR));
            self.head = HISTORY - 1;
            self.rows_written = 0;
            self.smooth = sweep.pixels.iter().map(|&p| f32::from(p)).collect();
        }
        self.head = (self.head + 1) % HISTORY;
        self.raw[self.head * w..(self.head + 1) * w].copy_from_slice(&sweep.pixels);
        let row: Vec<Color32> = sweep.pixels.iter().map(|&p| self.lut[p.min(AMP_MAX) as usize]).collect();
        if let Some(t) = &mut self.texture {
            t.set_partial([0, self.head], ColorImage::new([w, 1], row), TextureOptions::LINEAR);
        }
        let a = smoothing.clamp(0.0, 0.95);
        for (s, &p) in self.smooth.iter_mut().zip(&sweep.pixels) {
            *s = *s * a + f32::from(p) * (1.0 - a);
        }
        self.rows_written += 1;
        self.last = Some(sweep);
    }

    /// Draw the spectrum, frequency scale and waterfall in the available space.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        d: &mut DisplaySettings,
        theme: AppTheme,
        tuned: Option<(u64, ModeSetting)>,
        step_hz: u64,
    ) -> PlotAction {
        let mut action = PlotAction::default();
        let full = ui.available_rect_before_wrap();
        let (rect, resp) = ui.allocate_exact_size(full.size().max(vec2(200.0, 200.0)), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme.plot_bg());
        let Some(sweep) = self.last.clone() else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No scope data yet",
                egui::FontId::proportional(16.0),
                theme.scale_text(),
            );
            return action;
        };
        let axis_w = 34.0;
        let scale_h = 20.0;
        let plot_x = rect.min.x + axis_w;
        let spec_h = ((rect.height() - scale_h) * d.spectrum_share.clamp(0.15, 0.85)).round();
        let spec = Rect::from_min_max(pos2(plot_x, rect.min.y + 8.0), pos2(rect.max.x, rect.min.y + spec_h));
        let scale = Rect::from_min_max(pos2(plot_x, spec.max.y), pos2(rect.max.x, spec.max.y + scale_h));
        let fall = Rect::from_min_max(pos2(plot_x, scale.max.y), rect.max);
        let span = sweep.high_hz.saturating_sub(sweep.low_hz).max(1);
        let x_of = |hz: f64| plot_x + spec.width() * ((hz - sweep.low_hz as f64) / span as f64) as f32;
        let hz_at = |x: f32| sweep.low_hz as f64 + f64::from((x - plot_x) / spec.width()).clamp(0.0, 1.0) * span as f64;
        let small = egui::FontId::monospace(10.5);

        // Level axis (radio scope units 0–160; the IC-7300 does not report dB over CI-V).
        for lvl in (0..=160).step_by(20) {
            let y = spec.max.y - spec.height() * lvl as f32 / f32::from(AMP_MAX);
            painter.line_segment([pos2(plot_x, y), pos2(spec.max.x, y)], Stroke::new(1.0, theme.grid()));
            if lvl % 40 == 0 {
                painter.text(
                    pos2(rect.min.x + axis_w - 4.0, y),
                    egui::Align2::RIGHT_CENTER,
                    lvl.to_string(),
                    small.clone(),
                    theme.scale_text(),
                );
            }
        }

        // Frequency grid: pick a round step giving at most 12 lines.
        let step = [1_000u64, 2_000, 2_500, 5_000, 10_000, 20_000, 25_000, 50_000, 100_000, 200_000]
            .into_iter()
            .find(|s| span / s <= 12)
            .unwrap_or(250_000);
        let mut f = sweep.low_hz.div_ceil(step) * step;
        painter.rect_filled(scale, 0.0, theme.plot_bg().gamma_multiply(0.9));
        while f <= sweep.high_hz {
            let x = x_of(f as f64);
            painter.line_segment([pos2(x, spec.min.y), pos2(x, spec.max.y)], Stroke::new(1.0, theme.grid()));
            painter.line_segment([pos2(x, scale.min.y), pos2(x, scale.min.y + 5.0)], Stroke::new(1.0, theme.scale_text()));
            let label = if step < 10_000 { format!("{:.3}", f as f64 / 1e6) } else { format!("{:.2}", f as f64 / 1e6) };
            painter.text(pos2(x, scale.center().y + 2.0), egui::Align2::CENTER_CENTER, label, small.clone(), theme.scale_text());
            f += step;
        }

        // Passband of the tuned frequency.
        if let Some((hz, m)) = tuned
            && d.show_passband
        {
            let (lo, hi) = passband(m);
            let x0 = x_of(hz as f64 + lo as f64).clamp(plot_x, rect.max.x);
            let x1 = x_of(hz as f64 + hi as f64).clamp(plot_x, rect.max.x);
            if x1 > x0 {
                painter.rect_filled(
                    Rect::from_min_max(pos2(x0, spec.min.y), pos2(x1, spec.max.y)),
                    0.0,
                    theme.accent().gamma_multiply(0.18),
                );
            }
        }

        // Spectrum trace with a gradient fill.
        if !self.smooth.is_empty() {
            let n = self.smooth.len();
            let pts: Vec<Pos2> = self
                .smooth
                .iter()
                .enumerate()
                .map(|(i, &v)| {
                    pos2(
                        plot_x + spec.width() * i as f32 / (n - 1).max(1) as f32,
                        spec.max.y - spec.height() * (v / f32::from(AMP_MAX)).min(1.0),
                    )
                })
                .collect();
            if d.fill {
                let mut mesh = Mesh::default();
                let top = theme.trace().gamma_multiply(0.55);
                let bottom = theme.trace().gamma_multiply(0.05);
                for p in &pts {
                    let t = ((spec.max.y - p.y) / spec.height()).clamp(0.0, 1.0);
                    let c = egui::lerp(egui::Rgba::from(bottom)..=egui::Rgba::from(top), t);
                    mesh.colored_vertex(*p, c.into());
                    mesh.colored_vertex(pos2(p.x, spec.max.y), bottom);
                }
                for i in 0..n.saturating_sub(1) as u32 {
                    let a = i * 2;
                    mesh.add_triangle(a, a + 1, a + 2);
                    mesh.add_triangle(a + 1, a + 3, a + 2);
                }
                painter.add(egui::Shape::mesh(mesh));
            }
            painter.add(egui::Shape::line(pts, Stroke::new(1.3, theme.trace())));
        }

        // Waterfall: newest row at the top, two quads over the ring buffer.
        if let Some(t) = &self.texture {
            let h = HISTORY as f32;
            let rows = self.rows_written.min(HISTORY) as f32;
            let row_h = fall.height() / h;
            let newest = (self.head + 1) as f32;
            let mut mesh = Mesh::with_texture(t.id());
            let top_rows = newest.min(rows);
            let r1 = Rect::from_min_size(fall.min, vec2(fall.width(), top_rows * row_h));
            mesh.add_rect_with_uv(r1, Rect::from_min_max(pos2(0.0, newest / h), pos2(1.0, (newest - top_rows) / h)), Color32::WHITE);
            let rest = (rows - top_rows).max(0.0);
            if rest > 0.0 {
                let r2 = Rect::from_min_size(pos2(fall.min.x, r1.max.y), vec2(fall.width(), rest * row_h));
                mesh.add_rect_with_uv(r2, Rect::from_min_max(pos2(0.0, 1.0), pos2(1.0, 1.0 - rest / h)), Color32::WHITE);
            }
            painter.add(egui::Shape::mesh(mesh));
        }

        // Tuned-frequency marker across all three areas.
        if let Some((hz, _)) = tuned
            && hz >= sweep.low_hz
            && hz <= sweep.high_hz
        {
            let x = x_of(hz as f64);
            painter.line_segment([pos2(x, spec.min.y), pos2(x, rect.max.y)], Stroke::new(1.2, theme.accent()));
            painter.add(egui::Shape::convex_polygon(
                vec![pos2(x - 5.0, spec.min.y), pos2(x + 5.0, spec.min.y), pos2(x, spec.min.y + 7.0)],
                theme.accent(),
                Stroke::NONE,
            ));
        }
        if sweep.out_of_range {
            painter.text(spec.center(), egui::Align2::CENTER_CENTER, "Out of range", egui::FontId::proportional(16.0), Color32::YELLOW);
        }

        // Splitter between spectrum and waterfall: drag the frequency scale.
        let splitter = ui.interact(scale, ui.scope_id().with("spectrum_splitter"), Sense::drag());
        if splitter.hovered() || splitter.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if splitter.dragged() {
            let dy = splitter.drag_delta().y;
            d.spectrum_share = ((spec_h + dy) / (rect.height() - scale_h)).clamp(0.15, 0.85);
        }

        // Hover readout, click to tune (snapped to the step), scroll to step.
        let snap = |hz: f64| -> u64 {
            let s = if d.click_round_hz > 0 { d.click_round_hz } else { step_hz.max(1) } as f64;
            ((hz / s).round() * s) as u64
        };
        if let Some(p) = resp.hover_pos()
            && p.x >= plot_x
            && !scale.contains(p)
        {
            painter.line_segment([pos2(p.x, spec.min.y), pos2(p.x, rect.max.y)], Stroke::new(1.0, Color32::from_white_alpha(70)));
            let label = format!("{:.4} MHz", snap(hz_at(p.x)) as f64 / 1e6);
            painter.text(
                pos2(p.x + 6.0, spec.min.y + 14.0),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::monospace(12.0),
                theme.trace(),
            );
            action.scroll = crate::app::Wheel::read(ui);
        }
        // Drag along the plot to slide the tuning, as on SDR consoles.
        let started_on_scale = ui.input(|i| i.pointer.press_origin()).is_some_and(|o| scale.contains(o));
        if resp.dragged_by(egui::PointerButton::Primary)
            && !started_on_scale
            && let Some(p) = resp.interact_pointer_pos()
            && p.x >= plot_x
        {
            action.tune_to = Some(snap(hz_at(p.x)));
        }
        // Peak under the pointer (click-to-signal), shown as a marker while hovering.
        let radius_frac = 12.0 / spec.width();
        let frac_of = |x: f32| (x - plot_x) / spec.width();
        if d.snap_to_signal
            && let Some(p) = resp.hover_pos()
            && p.x >= plot_x
            && !scale.contains(p)
        {
            let exact = snap(hz_at(p.x));
            if let Some(hz) = self.click_frequency(frac_of(p.x), d, step_hz, radius_frac)
                && hz != exact
            {
                let x = x_of(hz as f64);
                painter.circle_stroke(pos2(x, spec.min.y + 22.0), 5.0, Stroke::new(1.5, theme.accent()));
                painter.text(
                    pos2(x + 8.0, spec.min.y + 30.0),
                    egui::Align2::LEFT_CENTER,
                    format!("signal {:.4}", hz as f64 / 1e6),
                    egui::FontId::monospace(11.0),
                    theme.accent(),
                );
            }
        }
        if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
            && p.x >= plot_x
            && !scale.contains(p)
        {
            action.tune_to = self.click_frequency(frac_of(p.x), d, step_hz, radius_frac);
        }
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_map_is_brighter_at_the_top() {
        let lum = |c: Color32| u32::from(c.r()) * 3 + u32::from(c.g()) * 6 + u32::from(c.b());
        for m in ColourMap::ALL {
            assert!(lum(m.at(1.0)) > lum(m.at(0.0)), "{m:?}");
        }
    }

    #[test]
    fn floor_ceiling_and_gamma_shape_the_table() {
        let mut d = DisplaySettings { colour_map: ColourMap::Grayscale, floor: 40, ceiling: 120, gamma: 1.0, ..Default::default() };
        let lut = d.lut();
        assert_eq!(lut[0], Color32::BLACK);
        assert_eq!(lut[40], Color32::BLACK);
        assert_eq!(lut[120], Color32::WHITE);
        assert_eq!(lut[160], Color32::WHITE);
        let mid = lut[80].r();
        d.gamma = 2.0; // darker
        assert!(d.lut()[80].r() < mid);
        d.gamma = 0.5; // lighter
        assert!(d.lut()[80].r() > mid);
    }

    #[test]
    fn click_snaps_to_a_nearby_signal_only() {
        let mut s = vec![20.0f32; 475];
        s[200] = 90.0;
        s[199] = 60.0;
        s[201] = 60.0;
        assert_eq!(find_peak(&s, 205, 8), Some(200));
        assert_eq!(find_peak(&s, 300, 8), None, "no signal near the click: plain noise");
        let mut noise: Vec<f32> = (0..475).map(|i| 20.0 + (i % 7) as f32).collect();
        assert_eq!(find_peak(&noise, 100, 8), None);
        noise[103] = 33.0; // only 8 over the median: not a signal
        assert_eq!(find_peak(&noise, 100, 8), None);
    }

    #[test]
    fn clicks_round_to_whole_kilohertz_by_default() {
        let mut wf = Waterfall::default();
        let mut pixels = vec![20u8; 475];
        pixels[237] = 120; // a signal in the middle
        wf.last = Some(Sweep {
            mode: rigdeck_core::rig::ScopeMode::Fixed,
            low_hz: 7_200_000,
            high_hz: 7_223_200,
            out_of_range: false,
            pixels: pixels.clone(),
        });
        wf.smooth = pixels.iter().map(|&p| f32::from(p)).collect();
        let d = DisplaySettings::default();
        // Peak at 7.2116 MHz: tunes to 7.212, not 7.211600.
        assert_eq!(wf.click_frequency(0.505, &d, 100, 0.012), Some(7_212_000));
        // Empty spectrum: the click position, still a whole kHz.
        assert_eq!(wf.click_frequency(0.1, &d, 100, 0.012), Some(7_202_000));
        let fine = DisplaySettings { click_round_hz: 0, ..Default::default() };
        assert_eq!(wf.click_frequency(0.505, &fine, 100, 0.012), Some(7_211_600));
    }

    #[test]
    fn passband_sides() {
        let usb = passband(ModeSetting::new(Mode::Usb));
        assert!(usb.0 > 0 && usb.1 > usb.0);
        let lsb = passband(ModeSetting::new(Mode::Lsb));
        assert!(lsb.1 < 0);
        let am = passband(ModeSetting::new(Mode::Am));
        assert_eq!(am.0, -am.1);
    }
}
