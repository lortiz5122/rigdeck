//! Layout: a top bar with the frequency and S-meter, a sidebar of collapsible sections (radio,
//! mode, bands, tuning, memories, transmit, scope, display, servers, clock), the spectrum and
//! waterfall in the middle, and a status bar.

use super::Wheel;
use super::*;
use crate::memories::RowStatus;
use crate::theme::AppTheme;
use crate::waterfall::{ColourMap, DisplaySettings};
use rigdeck_core::memory::MemoryChannel;

/// IC-7300 scope half-spans in Hz (manual p. 19-14).
const SPANS: [u32; 8] = [2_500, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000];

const STEPS: [u64; 12] = [1, 10, 50, 100, 500, 1_000, 2_500, 5_000, 9_000, 10_000, 12_500, 25_000];

fn step_label(s: u64) -> String {
    if s >= 1_000 { format!("{} kHz", s as f64 / 1000.0) } else { format!("{s} Hz") }
}

fn section(ui: &mut egui::Ui, title: &str, open: bool, add: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).strong()).default_open(open).show(ui, add);
}

impl RigDeckApp {
    pub(super) fn layout(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.applied_theme != Some(self.settings.theme) {
            self.settings.theme.apply(&ctx);
            self.applied_theme = Some(self.settings.theme);
        }
        self.waterfall.apply_settings(&self.settings.display);
        let scale = self.settings.ui_scale.clamp(0.7, 1.6);
        if (ctx.zoom_factor() - scale).abs() > 0.001 {
            ctx.set_zoom_factor(scale);
        }
        if let Some(ch) = self.memories.tune_request.take() {
            self.tune_channel(&ctx, ch);
        }
        self.keyboard(&ctx);

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui, &ctx);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.settings.show_log {
            egui::Panel::bottom("log").resizable(true).default_size(120.0).show(ui, |ui| self.log_panel(ui));
        }
        if self.settings.show_sidebar {
            egui::Panel::left("sidebar").resizable(true).default_size(310.0).min_size(260.0).show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.sidebar(ui, &ctx));
            });
        }
        let has_scope = self.engine().is_some_and(|e| e.capabilities().has_scope);
        let frame = if has_scope { egui::Frame::NONE } else { egui::Frame::central_panel(&ctx.global_style()) };
        egui::CentralPanel::default().frame(frame).show(ui, |ui| self.plot(ui, &ctx));
        let engine = self.engine();
        self.memories.show(&ctx, engine, &self.log);
        self.ptt_modal(&ctx);
    }

    fn tune_channel(&self, ctx: &egui::Context, ch: MemoryChannel) {
        let Some(e) = self.engine() else { return };
        let kind = e.capabilities().kind;
        let label = format!("memory {} ({:.4} MHz {} {})", ch.label(kind), ch.freq_hz as f64 / 1e6, ch.mode.label(), ch.name);
        let log = self.log.clone();
        self.act(ctx, "Recall memory", move |e| {
            e.set_frequency(ch.freq_hz).map_err(|x| x.to_string())?;
            let mut m = ch.mode;
            if !e.capabilities().has_filters {
                m.filter = None;
            }
            e.set_mode(m).map_err(|x| x.to_string())?;
            log.push(format!("Tuned to {label}"));
            Ok(())
        });
    }

    /// Turn wheel movement into whole steps: every wheel notch is one step (none are dropped),
    /// Shift = 10 steps, Option = 100 steps, and fast spinning speeds up on its own.
    pub(super) fn wheel_to_steps(&self, w: Wheel) -> i64 {
        let acc = self.wheel_accum.get() + w.lines;
        let whole = acc.trunc();
        self.wheel_accum.set(acc - whole);
        let mut steps = whole as i64;
        if steps == 0 {
            return 0;
        }
        // Acceleration: only a fast spin (more than 15 notches in 300 ms) multiplies by 5.
        let now = Instant::now();
        let mut recent = self.wheel_recent.borrow_mut();
        recent.push_back((now, steps.abs()));
        while recent.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_millis(300)) {
            recent.pop_front();
        }
        let speed: i64 = recent.iter().map(|(_, n)| n).sum();
        if speed > 15 {
            steps *= 5;
        }
        if w.alt {
            steps *= 100;
        } else if w.shift {
            steps *= 10;
        }
        steps
    }

    pub(super) fn step_tune(&self, steps: i64) {
        let Some(e) = self.engine() else { return };
        let caps = e.capabilities();
        let Some(f) = self.shown_frequency() else { return };
        let s = self.settings.step_hz.max(1) as i64;
        let snapped = (f as i64 / s) * s;
        let base = if steps > 0 || snapped == f as i64 { snapped } else { snapped + s };
        let new = (base + steps * s).clamp(caps.min_freq_hz as i64, caps.max_freq_hz as i64) as u64;
        self.tune(new);
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (up, down, right, left) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::ArrowLeft),
            )
        });
        if up || right {
            self.step_tune(if up { 10 } else { 1 });
        }
        if down || left {
            self.step_tune(if down { -10 } else { -1 });
        }
    }

    // ------------------------------------------------------------------ top bar

    fn top_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new("☰").size(18.0)).frame(false)).on_hover_text("Show or hide the sidebar").clicked() {
                self.settings.show_sidebar = !self.settings.show_sidebar;
            }
            ui.add_space(6.0);
            let st = self.engine().map(|e| e.state());
            let caps = self.engine().map(|e| e.capabilities().clone());
            match (&st, &caps) {
                (Some(_), Some(caps)) => {
                    let f = self.shown_frequency();
                    self.frequency_display(ui, f, caps)
                }
                _ => {
                    ui.label(RichText::new("---.---.---").size(34.0).monospace().color(ui.visuals().weak_text_color()));
                }
            }
            ui.add_space(10.0);
            ui.vertical(|ui| {
                let mode = st.as_ref().and_then(|s| s.mode).map(|m| m.label()).unwrap_or_else(|| "—".into());
                ui.label(RichText::new(mode).size(16.0).strong());
                let step = self.settings.step_hz;
                ui.label(RichText::new(format!("step {}", step_label(step))).size(11.0).color(ui.visuals().weak_text_color()));
            });
            ui.add_space(12.0);
            ui.vertical(|ui| {
                let s = st.as_ref().and_then(|s| s.s_meter);
                ui.add(
                    egui::ProgressBar::new(f32::from(s.unwrap_or(0)) / 241.0)
                        .desired_width(230.0)
                        .text(RichText::new(s.map(s_meter_text).unwrap_or_else(|| "S".into())).monospace()),
                );
                let scale = "S1   3   5   7   9  +20 +40 +60";
                ui.label(RichText::new(scale).size(10.0).monospace().color(ui.visuals().weak_text_color()));
            });
            if st.as_ref().is_some_and(|s| s.tx) {
                ui.add_space(8.0);
                ui.label(RichText::new(" ON AIR ").strong().color(Color32::WHITE).background_color(Color32::from_rgb(200, 30, 30)));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.toggle_value(&mut self.settings.show_log, "Log");
                ui.toggle_value(&mut self.memories.open, "Memory editor");
                let (dot, text) = match self.engine() {
                    Some(e) if e.is_running() => {
                        (Color32::from_rgb(80, 200, 120), format!("{} · {}", e.capabilities().kind.name(), e.link()))
                    }
                    Some(e) => (Color32::from_rgb(230, 90, 80), format!("Link lost: {}", e.state().link_error.unwrap_or_default())),
                    None if self.want_link => (Color32::from_rgb(240, 180, 60), "Waiting for the radio…".into()),
                    None => (Color32::GRAY, "Not connected".into()),
                };
                ui.label(text);
                ui.colored_label(dot, "●");
                if self.engine().is_none() && !self.want_link && ui.button(RichText::new("Connect").strong()).clicked() {
                    self.connect(ctx);
                }
            });
        });
    }

    fn frequency_display(&mut self, ui: &mut egui::Ui, hz: Option<u64>, caps: &rigdeck_core::Capabilities) {
        let Some(hz) = hz else {
            ui.label(RichText::new("---.---.---").size(34.0).monospace());
            return;
        };
        let digits = format!("{hz:010}");
        let mut delta: i64 = 0;
        let strong = ui.visuals().strong_text_color();
        let weak = ui.visuals().weak_text_color();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let lead = digits.find(|c| c != '0').unwrap_or(9).min(6);
            for (i, ch) in digits.chars().enumerate() {
                if i < lead {
                    continue;
                }
                let place = 9 - i as u32;
                let r = ui
                    .add(egui::Label::new(RichText::new(ch.to_string()).size(34.0).monospace().color(strong)).sense(egui::Sense::click()))
                    .on_hover_text("Scroll, click (up) or right-click (down) to change this digit");
                if r.hovered() {
                    ui.painter()
                        .line_segment([r.rect.left_bottom(), r.rect.right_bottom()], egui::Stroke::new(2.0, self.settings.theme.accent()));
                    if let Some(w) = Wheel::read(ui) {
                        // Plain notches on a digit: one unit of that digit each, none dropped.
                        let acc = self.wheel_accum.get() + w.lines;
                        let whole = acc.trunc();
                        self.wheel_accum.set(acc - whole);
                        delta += 10i64.pow(place) * whole as i64;
                    }
                }
                if r.clicked() {
                    delta += 10i64.pow(place);
                }
                if r.secondary_clicked() {
                    delta -= 10i64.pow(place);
                }
                if place == 6 || place == 3 {
                    ui.label(RichText::new(".").size(34.0).monospace().color(weak));
                }
            }
        });
        if delta != 0 {
            let new = (hz as i64 + delta).clamp(caps.min_freq_hz as i64, caps.max_freq_hz as i64) as u64;
            self.tune(new);
        }
    }

    // ------------------------------------------------------------------ sidebar

    fn sidebar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(4.0);
        section(ui, "Radio", true, |ui| self.radio_section(ui, ctx));
        let engine = self.engine();
        if let Some(e) = &engine {
            let caps = e.capabilities().clone();
            let st = e.state();
            section(ui, "Mode and filter", true, |ui| self.mode_section(ui, ctx, &caps, &st));
            section(ui, "Bands", true, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for b in &caps.bands {
                        let here = st.frequency_hz.is_some_and(|f| f >= b.start_hz && f <= b.end_hz);
                        if ui.selectable_label(here, b.name).clicked() {
                            self.tune(b.default_hz);
                        }
                    }
                });
            });
        }
        section(ui, "Tuning", true, |ui| self.tuning_section(ui));
        section(ui, "Memories", true, |ui| self.memory_section(ui, ctx));
        if let Some(e) = &engine {
            let caps = e.capabilities().clone();
            let st = e.state();
            section(ui, "Transmit", false, |ui| self.transmit_section(ui, ctx, &st));
            if caps.has_scope {
                section(ui, "Scope", true, |ui| self.scope_section(ui, ctx, &st));
            }
        }
        section(ui, "Display", false, |ui| self.display_section(ui));
        section(ui, "Network servers", false, |ui| self.servers_section(ui));
        if engine.as_ref().is_some_and(|e| e.capabilities().has_clock) {
            section(ui, "Radio clock", false, |ui| {
                ui.label("Waits for the top of the minute, then sets the radio's time and date to UTC from this Mac.");
                if ui.button("Set the radio clock to UTC").clicked() {
                    self.act(ctx, "Clock", sync_clock_utc);
                }
            });
        }
        section(ui, "About", false, |ui| {
            ui.label(format!("RigDeck {}", env!("CARGO_PKG_VERSION")));
            ui.label("Icom IC-7300 (CI-V) and Yaesu FT-991A (CAT). Written from the manufacturers' manuals.");
            ui.label(format!("Memory backups: {}", backup_dir().display()));
        });
    }

    fn radio_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.engine().is_some() || self.want_link;
        ui.add_enabled_ui(!busy, |ui| {
            egui::Grid::new("radio_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Radio");
                egui::ComboBox::from_id_salt("rig").selected_text(self.settings.rig.name()).width(180.0).show_ui(ui, |ui| {
                    for k in RigKind::ALL {
                        if ui.selectable_value(&mut self.settings.rig, k, k.name()).clicked() {
                            self.settings.baud = k.default_baud();
                            if let Some(p) = self.ports.iter().find(|p| p.guess == Some(k)) {
                                self.settings.port = PortChoice::Path(p.path.clone());
                            }
                        }
                    }
                });
                ui.end_row();
                ui.label("Port");
                ui.horizontal(|ui| {
                    let label = match &self.settings.port {
                        PortChoice::Simulator => "Built-in simulator".to_string(),
                        PortChoice::Path(p) => p.trim_start_matches("/dev/").to_string(),
                    };
                    egui::ComboBox::from_id_salt("port").selected_text(label).width(150.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.settings.port, PortChoice::Simulator, "Built-in simulator");
                        for p in &self.ports {
                            let l = match p.guess {
                                Some(k) => format!("{}  ({})", p.path, k.name()),
                                None => p.path.clone(),
                            };
                            ui.selectable_value(&mut self.settings.port, PortChoice::Path(p.path.clone()), l);
                        }
                    });
                    if ui.small_button("⟳").on_hover_text("Rescan serial ports").clicked() {
                        self.ports = discovery::list_ports();
                    }
                });
                ui.end_row();
                ui.label("Speed");
                egui::ComboBox::from_id_salt("baud").selected_text(format!("{} bps", self.settings.baud)).width(180.0).show_ui(ui, |ui| {
                    for b in [4_800u32, 9_600, 19_200, 38_400, 57_600, 115_200] {
                        ui.selectable_value(&mut self.settings.baud, b, format!("{b} bps"));
                    }
                });
                ui.end_row();
            });
        });
        ui.horizontal(|ui| {
            if busy {
                if ui.button("Disconnect").clicked() {
                    self.want_link = false;
                    self.last_open_error = None;
                    self.disconnect();
                }
            } else if ui.button(RichText::new("Connect").strong()).clicked() {
                self.connect(ctx);
            }
            if let Some(e) = self.engine() {
                let st = e.state();
                if let Some(id) = st.id {
                    ui.label(match e.capabilities().kind {
                        RigKind::IcomIc7300 => format!("CI-V {id:02X}h"),
                        RigKind::YaesuFt991a => format!("ID {id:04}"),
                    });
                }
            }
        });
        ui.checkbox(&mut self.settings.auto_reconnect, "Reconnect automatically if the link drops");
        ui.checkbox(&mut self.settings.read_memories_on_connect, "Read memories when connecting");
    }

    fn mode_section(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        caps: &rigdeck_core::Capabilities,
        st: &rigdeck_core::engine::RadioState,
    ) {
        let cur = st.mode;
        ui.horizontal_wrapped(|ui| {
            for m in &caps.modes {
                let sel = cur.is_some_and(|c| c.mode == *m);
                if ui.selectable_label(sel, m.label()).clicked() {
                    let filter = cur.and_then(|c| c.filter);
                    let ms = ModeSetting { mode: *m, filter, data: false };
                    self.act(ctx, "Mode", move |e| e.set_mode(ms).map(|_| ()).map_err(|x| x.to_string()));
                }
            }
        });
        if let Some(c) = cur {
            ui.horizontal(|ui| {
                let can_data = matches!(c.mode, Mode::Lsb | Mode::Usb | Mode::Fm | Mode::Am);
                if ui
                    .add_enabled(can_data, egui::Button::new("DATA").selected(c.data))
                    .on_hover_text("Data mode (USB-D for FT8 and other digital modes)")
                    .clicked()
                {
                    let ms = ModeSetting { data: !c.data, ..c };
                    self.act(ctx, "DATA", move |e| e.set_mode(ms).map(|_| ()).map_err(|x| x.to_string()));
                }
                if caps.has_filters {
                    ui.separator();
                    for f in 1..=3u8 {
                        if ui.selectable_label(c.filter == Some(f), format!("FIL{f}")).clicked() {
                            let ms = ModeSetting { filter: Some(f), ..c };
                            self.act(ctx, "Filter", move |e| e.set_mode(ms).map(|_| ()).map_err(|x| x.to_string()));
                        }
                    }
                }
            });
        }
    }

    fn tuning_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Step");
            egui::ComboBox::from_id_salt("step").selected_text(step_label(self.settings.step_hz)).show_ui(ui, |ui| {
                for s in STEPS {
                    ui.selectable_value(&mut self.settings.step_hz, s, step_label(s));
                }
            });
            if ui.button("◀").on_hover_text("Down one step (←)").clicked() {
                self.step_tune(-1);
            }
            if ui.button("▶").on_hover_text("Up one step (→)").clicked() {
                self.step_tune(1);
            }
        });
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.freq_entry).hint_text("MHz, e.g. 14.074").desired_width(140.0));
            if ((r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) || ui.button("Go").clicked())
                && let Ok(mhz) = self.freq_entry.trim().parse::<f64>()
            {
                self.tune((mhz * 1e6).round() as u64);
                self.freq_entry.clear();
            }
        });
        ui.label(
            RichText::new("Arrow keys: ←/→ one step, ↑/↓ ten steps. Scroll on the waterfall or on a digit.")
                .size(11.0)
                .color(ui.visuals().weak_text_color()),
        );
    }

    fn memory_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let engine = self.engine();
        let kind = engine.as_ref().map(|e| e.capabilities().kind).or(self.memories.kind).unwrap_or(self.settings.rig);
        ui.horizontal(|ui| {
            if ui.add_enabled(engine.is_some(), egui::Button::new("Read all")).clicked()
                && let Some(e) = engine.clone()
            {
                self.memories.read_all(e, ctx.clone());
            }
            if ui.button("Editor…").on_hover_text("Edit, import and export channels").clicked() {
                self.memories.open = true;
            }
        });
        {
            let j = self.memories.job.lock().unwrap();
            if j.total > 0 && !j.finished {
                ui.add(egui::ProgressBar::new(j.done as f32 / j.total as f32).text(format!("{} {}/{}", j.label, j.done, j.total)));
            }
        }
        if let Some(e) = engine.clone() {
            let caps = e.capabilities().clone();
            let st = e.state();
            ui.horizontal(|ui| {
                ui.label("Store as");
                ui.add(
                    egui::DragValue::new(&mut self.store_channel)
                        .range(*caps.memory_channels.start()..=(*caps.memory_channels.end()).min(99))
                        .prefix("ch "),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.store_name).hint_text("name").char_limit(caps.memory_name_len).desired_width(90.0),
                );
                let can = st.frequency_hz.is_some() && st.mode.is_some();
                if ui
                    .add_enabled(can, egui::Button::new("Store"))
                    .on_hover_text("Backs up every channel, writes this one and reads it back")
                    .clicked()
                    && let (Some(f), Some(m)) = (st.frequency_hz, st.mode)
                {
                    let mut ch = MemoryChannel::new(self.store_channel, f, m.mode, &self.store_name);
                    ch.mode = m;
                    self.memories.write(e.clone(), ctx.clone(), vec![ch], self.log.clone());
                }
            });
        }
        let tuned = engine.as_ref().and_then(|e| e.state().frequency_hz);
        let rows: Vec<MemoryChannel> = self
            .memories
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| !r.ch.empty && r.status != RowStatus::Edited)
            .map(|r| r.ch.clone())
            .collect();
        if rows.is_empty() {
            ui.label(RichText::new("No channels read yet.").color(ui.visuals().weak_text_color()));
            return;
        }
        ui.label(RichText::new("Click a channel to tune the radio to it.").size(11.0).color(ui.visuals().weak_text_color()));
        egui::ScrollArea::vertical().id_salt("memory_list").max_height(260.0).show(ui, |ui| {
            for ch in rows {
                let mode = if ch.mode.data { format!("{}-D", ch.mode.mode.label()) } else { ch.mode.mode.label().to_string() };
                let text = format!("{:>4}  {:>10.4}  {:<6} {}", ch.label(kind), ch.freq_hz as f64 / 1e6, mode, ch.name);
                let here = tuned == Some(ch.freq_hz);
                let r = ui
                    .add_enabled(engine.is_some(), egui::Button::selectable(here, RichText::new(text).monospace().size(12.0)))
                    .on_hover_text("Tune to this channel");
                if r.clicked() {
                    self.tune_channel(ctx, ch);
                }
            }
        });
    }

    fn transmit_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, st: &rigdeck_core::engine::RadioState) {
        ui.horizontal(|ui| {
            let mut p = self.pending_power.or(st.power_pct).unwrap_or(0);
            ui.label("Power");
            let r = ui.add(egui::Slider::new(&mut p, 0..=100).suffix(" %"));
            if r.changed() {
                self.pending_power = Some(p);
            }
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.pending_power = None;
                self.act(ctx, "Power", move |e| e.set_power(p).map(|_| ()).map_err(|x| x.to_string()));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Unkey automatically after");
            ui.add(egui::DragValue::new(&mut self.settings.tx_limit_secs).range(5..=600).suffix(" s"));
        });
        match self.ptt {
            Ptt::Keyed { until } => {
                let left = until.saturating_duration_since(Instant::now()).as_secs();
                let unkey = ui.add(
                    egui::Button::new(RichText::new(format!("■  UNKEY  ({left} s)")).strong().color(Color32::WHITE))
                        .fill(Color32::from_rgb(190, 30, 30))
                        .min_size(egui::vec2(200.0, 30.0)),
                );
                if unkey.clicked() || (!st.tx && left + 1 < u64::from(self.settings.tx_limit_secs)) {
                    self.ptt = Ptt::Idle;
                    self.act(ctx, "Unkey", |e| e.transmit(false, Duration::ZERO).map(|_| ()).map_err(|x| x.to_string()));
                }
            }
            _ => {
                if ui.add(egui::Button::new("Transmit…").min_size(egui::vec2(200.0, 30.0))).clicked() {
                    self.ptt = Ptt::Confirm;
                }
            }
        }
        if st.tx {
            for (name, v, full) in [("Po", st.po, 213.0), ("SWR", st.swr, 120.0), ("ALC", st.alc, 120.0)] {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{name:<4}")).monospace());
                    ui.add(egui::ProgressBar::new(f32::from(v.unwrap_or(0)) / full).desired_width(200.0));
                });
            }
        }
    }

    fn ptt_modal(&mut self, ctx: &egui::Context) {
        if !matches!(self.ptt, Ptt::Confirm) {
            return;
        }
        let Some(e) = self.engine() else {
            self.ptt = Ptt::Idle;
            return;
        };
        let st = e.state();
        let limit = self.settings.tx_limit_secs;
        let f = st.frequency_hz.map(|f| format!("{:.6} MHz", f as f64 / 1e6)).unwrap_or_default();
        let m = st.mode.map(|m| m.label()).unwrap_or_default();
        let resp = egui::Modal::new(egui::Id::unique("ptt_confirm")).show(ctx, |ui| {
            ui.heading("Transmit?");
            ui.label(format!("The radio will transmit on {f} {m} at {}% power.", st.power_pct.unwrap_or(0)));
            ui.label(format!("It unkeys automatically after {limit} seconds, on Unkey, or if the link drops."));
            ui.label("Check the antenna and that you are licensed for this frequency.");
            let mut go = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                go = ui.add(egui::Button::new(RichText::new("Transmit").strong()).fill(Color32::from_rgb(170, 40, 40))).clicked();
                cancel = ui.button("Cancel").clicked();
            });
            (go, cancel)
        });
        let (go, cancel) = resp.inner;
        if go {
            let lim = Duration::from_secs(u64::from(limit));
            self.ptt = Ptt::Keyed { until: Instant::now() + lim };
            self.act(ctx, "Transmit", move |e| e.transmit(true, lim).map(|_| ()).map_err(|x| x.to_string()));
        } else if cancel || resp.should_close() {
            self.ptt = Ptt::Idle;
        }
    }

    fn scope_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, st: &rigdeck_core::engine::RadioState) {
        ui.horizontal(|ui| {
            let mut on = self.settings.scope;
            if ui.checkbox(&mut on, "Scope on").changed() {
                self.settings.scope = on;
                self.act(ctx, "Scope", move |e| {
                    e.request(Request::ScopeOn(on))
                        .and_then(|_| e.request(Request::ScopeDataOutput(on)))
                        .map(|_| ())
                        .map_err(|x| x.to_string())
                });
            }
            let shown = self
                .waterfall
                .last
                .as_ref()
                .map(|s| ((s.high_hz - s.low_hz) / 2) as u32)
                .filter(|s| *s > 0)
                .unwrap_or(self.settings.span_hz);
            egui::ComboBox::from_id_salt("span").selected_text(format!("±{} kHz", f64::from(shown) / 1000.0)).show_ui(ui, |ui| {
                for s in [2_500u32, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000] {
                    if ui.selectable_value(&mut self.settings.span_hz, s, format!("±{} kHz", f64::from(s) / 1000.0)).clicked() {
                        self.act(ctx, "Span", move |e| e.request(Request::ScopeSetSpan(s)).map(|_| ()).map_err(|x| x.to_string()));
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label("Mode");
            let cur = self.waterfall.last.as_ref().map(|s| s.mode);
            for (m, name) in [(rigdeck_core::rig::ScopeMode::Center, "Center"), (rigdeck_core::rig::ScopeMode::Fixed, "Fixed")] {
                if ui.selectable_label(cur == Some(m), name).clicked() {
                    self.act(ctx, "Scope mode", move |e| e.request(Request::ScopeSetMode(m)).map(|_| ()).map_err(|x| x.to_string()));
                }
            }
            ui.separator();
            ui.label("Speed");
            for (v, name) in [(0u8, "Fast"), (1, "Mid"), (2, "Slow")] {
                if ui.small_button(name).clicked() {
                    self.act(ctx, "Scope speed", move |e| e.request(Request::ScopeSetSpeed(v)).map(|_| ()).map_err(|x| x.to_string()));
                }
            }
        });
        let rate = self.sweep_times.len() as f32 / 2.0;
        ui.label(
            RichText::new(format!("{rate:.0} sweeps/s · {} received · {} dropped", st.stats.sweeps, st.stats.sweeps_dropped)).size(11.0),
        );
    }

    fn display_section(&mut self, ui: &mut egui::Ui) {
        let d = &mut self.settings.display;
        egui::Grid::new("display_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Interface size");
            ui.add(
                egui::Slider::new(&mut self.settings.ui_scale, 0.7..=1.6)
                    .step_by(0.05)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
            );
            ui.end_row();
            ui.label("Theme");
            egui::ComboBox::from_id_salt("theme").selected_text(self.settings.theme.name()).height(420.0).show_ui(ui, |ui| {
                for t in AppTheme::ALL {
                    if ui.selectable_value(&mut self.settings.theme, t, t.name()).clicked() && t == AppTheme::NightRed {
                        // Keep night vision: a red waterfall goes with the red theme.
                        d.colour_map = ColourMap::NightRed;
                    }
                }
            });
            ui.end_row();
            ui.label("Waterfall colours");
            egui::ComboBox::from_id_salt("cmap").selected_text(d.colour_map.name()).show_ui(ui, |ui| {
                for m in ColourMap::ALL {
                    ui.horizontal(|ui| {
                        gradient_swatch(ui, m, 60.0);
                        ui.selectable_value(&mut d.colour_map, m, m.name());
                    });
                }
            });
            ui.end_row();
            ui.label("");
            gradient_swatch(ui, d.colour_map, 180.0);
            ui.end_row();
            ui.label("Floor");
            let mut floor = d.floor;
            ui.add(egui::Slider::new(&mut floor, 0..=150))
                .on_hover_text("Signals below this level are drawn in the darkest colour. Raise it to hide noise.");
            d.floor = floor.min(d.ceiling.saturating_sub(5));
            ui.end_row();
            ui.label("Ceiling");
            let mut ceil = d.ceiling;
            ui.add(egui::Slider::new(&mut ceil, 10..=160))
                .on_hover_text("Signals above this level are drawn in the brightest colour. Lower it to make weak signals brighter.");
            d.ceiling = ceil.max(d.floor + 5);
            ui.end_row();
            ui.label("Brightness");
            // Shown as lighter to the right: the slider value is 1/gamma.
            let mut bright = 1.0 / d.gamma.clamp(0.2, 4.0);
            ui.add(egui::Slider::new(&mut bright, 0.25..=4.0).logarithmic(true).custom_formatter(|v, _| {
                if (v - 1.0).abs() < 0.05 {
                    "normal".into()
                } else if v < 1.0 {
                    format!("darker {:.1}×", 1.0 / v)
                } else {
                    format!("lighter {v:.1}×")
                }
            }));
            d.gamma = 1.0 / bright;
            ui.end_row();
            ui.label("Spectrum height");
            ui.add(egui::Slider::new(&mut d.spectrum_share, 0.15..=0.85).show_value(false));
            ui.end_row();
            ui.label("Averaging");
            ui.add(egui::Slider::new(&mut d.smoothing, 0.0..=0.9).show_value(false));
            ui.end_row();
        });
        ui.checkbox(&mut d.fill, "Fill under the spectrum");
        ui.checkbox(&mut d.show_passband, "Show the receive passband");
        ui.checkbox(&mut d.snap_to_signal, "Click on a signal tunes to its peak");
        ui.horizontal(|ui| {
            ui.label("Clicks tune to the nearest");
            let label = |v: u64| match v {
                0 => "tuning step".to_string(),
                v if v >= 1000 => format!("{} kHz", v as f64 / 1000.0),
                v => format!("{v} Hz"),
            };
            egui::ComboBox::from_id_salt("click_round").selected_text(label(d.click_round_hz)).show_ui(ui, |ui| {
                for v in [1_000u64, 500, 100, 5_000, 10_000, 0] {
                    ui.selectable_value(&mut d.click_round_hz, v, label(v));
                }
            });
        });
        if ui.button("Reset display settings").clicked() {
            *d = DisplaySettings::default();
        }
    }

    fn servers_section(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        changed |= ui.checkbox(&mut self.settings.rigctld_enabled, "rigctld (WSJT-X: \"Hamlib NET rigctl\")").changed();
        ui.horizontal(|ui| {
            ui.label("Port");
            changed |= ui.add(egui::DragValue::new(&mut self.settings.rigctld_port).range(1024..=65535)).changed();
        });
        ui.horizontal(|ui| {
            changed |= ui.checkbox(&mut self.settings.tci_enabled, "TCI, port").changed();
            changed |= ui.add(egui::DragValue::new(&mut self.settings.tci_port).range(1024..=65535)).changed();
        });
        changed |= ui.checkbox(&mut self.settings.rigctld_lan, "Allow other computers on the network").changed();
        ui.add_enabled_ui(self.settings.rigctld_lan, |ui| {
            ui.label("Allowed addresses");
            changed |=
                ui.add(egui::TextEdit::singleline(&mut self.settings.rigctld_allow).hint_text("192.168.1.20, 192.168.1.21")).lost_focus();
        });
        changed |= ui.checkbox(&mut self.settings.rigctld_allow_tx, "Allow clients to transmit (PTT)").changed();
        if let Some(s) = &self.rigctld {
            ui.label(format!("rigctld on {} · {} connections", s.local_addr, s.clients_served.load(std::sync::atomic::Ordering::Relaxed)));
        }
        if let Some(t) = &self.tci {
            ui.label(format!("TCI on ws://{}", t.local_addr));
        }
        if let Some(e) = &self.rigctld_error {
            ui.colored_label(Color32::from_rgb(230, 90, 80), e);
        }
        if changed {
            self.apply_rigctld();
        }
    }

    // ------------------------------------------------------------------ centre and status

    fn plot(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(e) = self.engine() else {
            ui.add_space(80.0);
            ui.vertical_centered(|ui| {
                ui.heading("Not connected");
                ui.label("Choose a radio and a port in the sidebar, then Connect. The built-in simulator works without a radio.");
                if !self.want_link && ui.button(RichText::new("Connect").strong()).clicked() {
                    self.connect(ctx);
                }
            });
            return;
        };
        let caps = e.capabilities().clone();
        let st = e.state();
        if !caps.has_scope {
            ui.add_space(80.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(format!("The {} does not send its spectrum display over its control port.", caps.kind.name()))
                        .color(ui.visuals().weak_text_color()),
                );
                ui.label(RichText::new("Use the sidebar for tuning, memories and transmit.").color(ui.visuals().weak_text_color()));
            });
            return;
        }
        self.span_bar(ui, ctx);
        let tuned = self.shown_frequency().zip(st.mode);
        let action = self.waterfall.show(ui, &mut self.settings.display, self.settings.theme, tuned, self.settings.step_hz);
        if let Some(hz) = action.tune_to {
            self.tune(hz);
        }
        if let Some(w) = action.scroll {
            if w.command {
                // ⌘ + scroll changes the visible width (the radio's scope span).
                let acc = self.wheel_accum.get() + w.lines;
                let whole = acc.trunc();
                self.wheel_accum.set(acc - whole);
                if whole != 0.0 {
                    self.zoom(ctx, if whole > 0.0 { -1 } else { 1 });
                }
            } else {
                let steps = self.wheel_to_steps(w);
                if steps != 0 {
                    self.step_tune(steps);
                }
            }
        }
    }

    /// Width of the visible spectrum: the radio's scope span (IC-7300 27 15: ±2.5 to ±500 kHz).
    fn span_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let current = self.waterfall.last.as_ref().map(|s| ((s.high_hz - s.low_hz) / 2) as u32).filter(|s| *s > 0);
        egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Width").size(12.0));
                if ui.small_button("−").on_hover_text("Narrower (⌘ + scroll up)").clicked() {
                    self.zoom(ctx, -1);
                }
                for s in SPANS {
                    let label = if s >= 1000 { format!("{}k", f64::from(s * 2) / 1000.0) } else { format!("{s}") };
                    if ui
                        .selectable_label(current == Some(s), RichText::new(label).size(12.0))
                        .on_hover_text(format!(
                            "Show {} kHz (±{} kHz around the tuned frequency)",
                            f64::from(s * 2) / 1000.0,
                            f64::from(s) / 1000.0
                        ))
                        .clicked()
                    {
                        self.set_span(ctx, s);
                    }
                }
                if ui.small_button("+").on_hover_text("Wider (⌘ + scroll down)").clicked() {
                    self.zoom(ctx, 1);
                }
                ui.separator();
                ui.label(
                    RichText::new("scroll: 1 step per notch · ⇧ ×10 · ⌥ ×100 · spin fast to speed up · ⌘ scroll: width")
                        .size(11.0)
                        .color(ui.visuals().weak_text_color()),
                );
            });
        });
    }

    fn set_span(&mut self, ctx: &egui::Context, s: u32) {
        self.settings.span_hz = s;
        self.act(ctx, "Span", move |e| e.request(Request::ScopeSetSpan(s)).map(|_| ()).map_err(|x| x.to_string()));
    }

    /// One span wider (+1) or narrower (-1).
    fn zoom(&mut self, ctx: &egui::Context, dir: i32) {
        let current = self.waterfall.last.as_ref().map(|s| ((s.high_hz - s.low_hz) / 2) as u32).unwrap_or(self.settings.span_hz);
        let i = SPANS.iter().position(|&s| s >= current).unwrap_or(SPANS.len() - 1) as i32;
        let j = (i + dir).clamp(0, SPANS.len() as i32 - 1) as usize;
        if SPANS[j] != current {
            self.set_span(ctx, SPANS[j]);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let small = |t: String| RichText::new(t).size(11.0);
            match self.engine() {
                Some(e) => {
                    let st = e.state().stats;
                    ui.label(small(format!(
                        "sent {} · replies {} · refused {} · timeouts {} · in {} kB · out {} kB",
                        st.sent,
                        st.messages,
                        st.refused,
                        st.timeouts,
                        st.bytes_in / 1024,
                        st.bytes_out / 1024
                    )));
                    if !st.backed_off.is_empty() {
                        let names: Vec<String> = st.backed_off.iter().map(|(n, d)| format!("{n} {}s", d.as_secs())).collect();
                        ui.label(small(format!("· backing off: {}", names.join(", "))))
                            .on_hover_text("Commands the radio refused in this mode are asked less often");
                    }
                }
                None => {
                    ui.label(small("idle".into()));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(l) = self.log.lines().last() {
                    ui.label(small(l.clone()));
                }
            });
        });
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            for l in self.log.lines().iter().rev().take(300).rev() {
                ui.label(RichText::new(l).monospace().size(11.0));
            }
        });
    }
}

fn gradient_swatch(ui: &mut egui::Ui, m: ColourMap, width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 12.0), egui::Sense::hover());
    let n = 32;
    for i in 0..n {
        let x0 = rect.min.x + rect.width() * i as f32 / n as f32;
        let x1 = rect.min.x + rect.width() * (i + 1) as f32 / n as f32;
        ui.painter().rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, rect.min.y), egui::pos2(x1 + 0.5, rect.max.y)),
            0.0,
            m.at(i as f32 / (n - 1) as f32),
        );
    }
}
