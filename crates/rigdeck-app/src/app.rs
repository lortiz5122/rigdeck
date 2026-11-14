//! The main window: state, connection handling and automation. The layout is in `app/ui.rs`.

mod ui;

use crate::memories::MemoryWindow;
use crate::waterfall::Waterfall;
use eframe::egui::{self, Color32, RichText};
use rigdeck_core::engine::{Engine, EngineConfig};
use rigdeck_core::rig::{Event, Mode, ModeSetting, Request, RigKind, s_meter_text};
use rigdeck_core::rigctld::{EngineSlot, RigctldConfig, RigctldServer};
use rigdeck_core::sim::{Ft991aSim, Ic7300Sim, SimHandle};
use rigdeck_core::transport::{SerialTransport, Transport, pipe_pair};
use rigdeck_core::{discovery, driver};
use serde::{Deserialize, Serialize};
use std::ops::Not;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// Mouse-wheel or trackpad movement this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wheel {
    /// In lines; trackpad points are converted at 20 points per line. Positive = up.
    pub lines: f32,
    pub shift: bool,
    pub alt: bool,
    pub command: bool,
}

impl Wheel {
    /// Read this frame's raw wheel events (not the smoothed delta, which merges and drops ticks).
    pub fn read(ui: &egui::Ui) -> Option<Wheel> {
        ui.input(|i| {
            let mut w = Wheel { lines: 0.0, shift: i.modifiers.shift, alt: i.modifiers.alt, command: i.modifiers.command };
            for e in &i.events {
                if let egui::Event::MouseWheel { unit, delta, .. } = e {
                    // Shift turns a vertical wheel into a horizontal one on macOS: count both axes.
                    let d = if delta.y.abs() >= delta.x.abs() { delta.y } else { delta.x };
                    w.lines += match unit {
                        egui::MouseWheelUnit::Line => d,
                        egui::MouseWheelUnit::Page => d * 10.0,
                        egui::MouseWheelUnit::Point => d / 20.0,
                    };
                }
            }
            (w.lines != 0.0).then_some(w)
        })
    }
}

/// Shared message log (UI and background jobs).
#[derive(Clone, Default)]
pub struct Shared(Arc<Mutex<Vec<String>>>);

impl Shared {
    pub fn push(&self, msg: impl Into<String>) {
        let line = format!("{}  {}", clock(), msg.into());
        let mut v = self.0.lock().unwrap();
        v.push(line);
        let n = v.len();
        if n > 500 {
            v.drain(..n - 500);
        }
    }
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// Local wall-clock time HH:MM:SS (the Mac's time zone).
pub fn clock() -> String {
    let t = std::process::Command::new("date").arg("+%H:%M:%S").output();
    t.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

pub fn stamp() -> String {
    let t = std::process::Command::new("date").arg("+%Y%m%d-%H%M%S").output();
    t.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

/// Visible folder for files the operator opens (memory backups, exports).
pub fn backup_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    // Not ~/Documents: that folder needs a privacy permission, and a refused permission would
    // stop backups. ~/RigDeck is visible in Finder and needs none.
    PathBuf::from(home).join("RigDeck").join("backups")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PortChoice {
    Simulator,
    Path(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub rig: RigKind,
    pub port: PortChoice,
    pub baud: u32,
    pub scope: bool,
    pub span_hz: u32,
    pub rigctld_enabled: bool,
    pub rigctld_port: u16,
    pub rigctld_lan: bool,
    pub rigctld_allow: String,
    pub rigctld_allow_tx: bool,
    pub tci_enabled: bool,
    pub tci_port: u16,
    pub tx_limit_secs: u32,
    pub theme: crate::theme::AppTheme,
    pub display: crate::waterfall::DisplaySettings,
    pub step_hz: u64,
    pub auto_reconnect: bool,
    pub read_memories_on_connect: bool,
    pub show_sidebar: bool,
    pub show_log: bool,
    /// Interface size (1.0 = normal).
    pub ui_scale: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            rig: RigKind::IcomIc7300,
            port: PortChoice::Simulator,
            baud: RigKind::IcomIc7300.default_baud(),
            scope: true,
            span_hz: 25_000,
            rigctld_enabled: false,
            rigctld_port: 4533,
            rigctld_lan: false,
            rigctld_allow: String::new(),
            rigctld_allow_tx: false,
            tci_enabled: false,
            tci_port: 40001,
            tx_limit_secs: 30,
            theme: crate::theme::AppTheme::Dark,
            display: crate::waterfall::DisplaySettings::default(),
            step_hz: 100,
            auto_reconnect: true,
            read_memories_on_connect: true,
            show_sidebar: true,
            show_log: false,
            ui_scale: 1.0,
        }
    }
}

/// Options from the command line, used for automated checks and screenshots.
#[derive(Debug, Default, Clone)]
pub struct LaunchOptions {
    pub simulator: Option<RigKind>,
    pub port: Option<String>,
    pub screenshot: Option<PathBuf>,
    pub screenshot_after: f32,
    pub open_memories: bool,
    pub read_memories: bool,
    pub disconnect_after: Option<f32>,
    pub exit_after: Option<f32>,
    pub cycles: u32,
    pub tune_memory: Option<u16>,
    /// Simulate a click on the spectrum at this share of its width (0..1).
    pub click_at: Option<f32>,
    /// Simulate this many wheel notches over the spectrum, one per frame (negative = down).
    pub scroll_notches: i32,
    pub theme: Option<crate::theme::AppTheme>,
    pub colour_map: Option<crate::waterfall::ColourMap>,
    pub gamma: Option<f32>,
}

enum Ptt {
    Idle,
    Confirm,
    Keyed { until: Instant },
}

pub struct RigDeckApp {
    settings: Settings,
    slot: EngineSlot,
    events: Option<Receiver<Event>>,
    tuner: Option<Sender<u64>>,
    sim: Option<Box<dyn std::any::Any + Send>>,
    rigctld: Option<RigctldServer>,
    rigctld_error: Option<String>,
    tci: Option<rigdeck_core::tci::TciServer>,
    closing: Option<std::thread::JoinHandle<()>>,
    log: Shared,
    ports: Vec<discovery::PortInfo>,
    waterfall: Waterfall,
    memories: MemoryWindow,
    freq_entry: String,
    store_channel: u16,
    store_name: String,
    applied_theme: Option<crate::theme::AppTheme>,
    ptt: Ptt,
    started: Instant,
    opts: LaunchOptions,
    screenshot_taken: bool,
    disconnected_by_option: bool,
    /// When the link drops (USB unplugged, radio off), retry the port every few seconds.
    next_reconnect: Option<Instant>,
    /// The operator wants to be connected (set by Connect, cleared by Disconnect).
    want_link: bool,
    last_open_error: Option<String>,
    cycles_done: u32,
    pending_power: Option<u16>,
    sweep_times: std::collections::VecDeque<Instant>,
    target: std::cell::Cell<Option<(u64, Instant)>>,
    wheel_accum: std::cell::Cell<f32>,
    wheel_recent: std::cell::RefCell<std::collections::VecDeque<(Instant, i64)>>,
    frames: u64,
    fps_since: Instant,
}

impl RigDeckApp {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: LaunchOptions) -> Self {
        let mut settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, "settings")).unwrap_or_default();
        if let Some(k) = opts.simulator {
            settings.rig = k;
            settings.port = PortChoice::Simulator;
            settings.baud = k.default_baud();
        }
        if let Some(p) = &opts.port {
            settings.port = PortChoice::Path(p.clone());
        }
        if let Some(t) = opts.theme {
            settings.theme = t;
        }
        if let Some(m) = opts.colour_map {
            settings.display.colour_map = m;
        }
        if let Some(g) = opts.gamma {
            settings.display.gamma = g;
        }
        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        cc.egui_ctx.set_global_style(style);
        let mut app = Self {
            settings,
            slot: Arc::new(RwLock::new(None)),
            events: None,
            tuner: None,
            sim: None,
            rigctld: None,
            rigctld_error: None,
            tci: None,
            closing: None,
            log: Shared::default(),
            ports: discovery::list_ports(),
            waterfall: Waterfall::default(),
            memories: MemoryWindow::default(),
            freq_entry: String::new(),
            store_channel: 1,
            store_name: String::new(),
            applied_theme: None,
            ptt: Ptt::Idle,
            started: Instant::now(),
            opts,
            screenshot_taken: false,
            disconnected_by_option: false,
            next_reconnect: None,
            want_link: false,
            last_open_error: None,
            cycles_done: 0,
            pending_power: None,
            sweep_times: Default::default(),
            target: Default::default(),
            wheel_accum: Default::default(),
            wheel_recent: Default::default(),
            frames: 0,
            fps_since: Instant::now(),
        };
        app.log.push(format!("RigDeck {} started", env!("CARGO_PKG_VERSION")));
        app.apply_rigctld();
        if app.opts.simulator.is_some() || app.opts.port.is_some() {
            app.connect(&cc.egui_ctx);
        }
        if app.opts.open_memories {
            app.memories.open = true;
        }
        app
    }

    fn engine(&self) -> Option<Arc<Engine>> {
        self.slot.read().unwrap().clone()
    }

    fn connected(&self) -> bool {
        self.engine().is_some_and(|e| e.is_running())
    }

    fn connect(&mut self, ctx: &egui::Context) {
        self.want_link = true;
        self.disconnect();
        // The port is released when the previous engine has stopped; wait for that (under a second).
        if let Some(t) = self.closing.take() {
            let _ = t.join();
        }
        let kind = self.settings.rig;
        let transport: Box<dyn Transport> = match &self.settings.port {
            PortChoice::Simulator => {
                let (a, b) = pipe_pair();
                match kind {
                    RigKind::IcomIc7300 => {
                        let mut s = Ic7300Sim::default();
                        for ch in rigdeck_core::sim::sample_memories(kind) {
                            s.store(&ch);
                        }
                        self.sim = Some(Box::new(SimHandle::spawn(s, Box::new(b))));
                    }
                    RigKind::YaesuFt991a => {
                        let mut s = Ft991aSim::default();
                        for ch in rigdeck_core::sim::sample_memories(kind) {
                            s.store(ch);
                        }
                        self.sim = Some(Box::new(SimHandle::spawn(s, Box::new(b))));
                    }
                }
                Box::new(a)
            }
            PortChoice::Path(p) => match SerialTransport::open(p, self.settings.baud) {
                Ok(t) => {
                    if self.last_open_error.take().is_some() {
                        self.log.push(format!("Reconnected to {p}"));
                    }
                    Box::new(t)
                }
                Err(e) => {
                    let msg = format!("Could not open {p}: {e}. Retrying every 3 s; press Disconnect to stop.");
                    println!("{msg}");
                    if self.last_open_error.as_deref() != Some(msg.as_str()) {
                        self.log.push(msg.clone());
                    }
                    self.last_open_error = Some(msg);
                    return;
                }
            },
        };
        let link = transport.describe();
        let p = driver(kind);
        let cfg = EngineConfig {
            default_tx_limit: Duration::from_secs(u64::from(self.settings.tx_limit_secs)),
            ..EngineConfig::default_for(&p.capabilities())
        };
        let engine = Arc::new(Engine::start(transport, p, cfg));
        self.events = Some(engine.subscribe());
        // Waker: repaint for each scope sweep (at most every 40 ms) and for other radio events at
        // most every 250 ms. Ends when the engine stops.
        let wake = engine.subscribe();
        let c4 = ctx.clone();
        std::thread::spawn(move || {
            let mut last = Instant::now() - Duration::from_secs(1);
            while let Ok(e) = wake.recv() {
                let mut sweep = matches!(e, Event::Sweep(_));
                while let Ok(more) = wake.try_recv() {
                    sweep |= matches!(more, Event::Sweep(_));
                }
                let gap = if sweep { Duration::from_millis(40) } else { Duration::from_millis(400) };
                let since = last.elapsed();
                if since < gap {
                    std::thread::sleep(gap - since);
                    while let Ok(more) = wake.try_recv() {
                        let _ = more;
                    }
                }
                c4.request_repaint();
                last = Instant::now();
            }
        });
        if self.memories.kind != Some(kind) {
            self.memories.rows.lock().unwrap().clear();
            self.memories.kind = Some(kind);
        }
        if self.settings.read_memories_on_connect && self.memories.rows.lock().unwrap().is_empty() {
            self.memories.read_all(engine.clone(), ctx.clone());
        }
        *self.slot.write().unwrap() = Some(engine.clone());
        self.waterfall.clear();
        // Tuner thread: always sends the latest requested frequency, so scrolling does not queue up.
        let (tx, rx) = std::sync::mpsc::channel::<u64>();
        self.tuner = Some(tx);
        let (e2, log2, c2) = (engine.clone(), self.log.clone(), ctx.clone());
        std::thread::spawn(move || {
            while let Ok(mut hz) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    hz = newer;
                }
                if let Err(err) = e2.set_frequency(hz) {
                    log2.push(format!("Tune to {hz} Hz: {err}"));
                }
                c2.request_repaint();
            }
        });
        let (log, scope, span, c3) = (self.log.clone(), self.settings.scope, self.settings.span_hz, ctx.clone());
        std::thread::spawn(move || {
            match engine.read_id() {
                Ok(id) => log.push(match kind {
                    RigKind::IcomIc7300 => format!("Connected to {} on {link} (CI-V ID {id:02X}h)", kind.name()),
                    RigKind::YaesuFt991a => format!("Connected to {} on {link} (ID {id:04})", kind.name()),
                }),
                Err(e) => log.push(format!("{} on {link} did not answer: {e}", kind.name())),
            }
            if engine.capabilities().has_scope && scope {
                // The span is left as the radio has it; the span menu changes it.
                let _ = span;
                let r = engine.request(Request::ScopeOn(true)).and_then(|_| engine.request(Request::ScopeDataOutput(true)));
                if let Err(e) = r {
                    log.push(format!(
                        "Scope: {e}. On the IC-7300 set CI-V USB Port to \"Unlink from [REMOTE]\" and CI-V USB Baud Rate to 115200."
                    ));
                }
            }
            c3.request_repaint();
        });
    }

    /// Disconnect through state: the engine is taken out of the slot and stopped on a background
    /// thread; windows keep their data and simply show "not connected".
    fn disconnect(&mut self) {
        let engine = self.slot.write().unwrap().take();
        self.events = None;
        self.tuner = None;
        self.ptt = Ptt::Idle;
        if let Some(e) = engine {
            let log = self.log.clone();
            let sim = self.sim.take();
            self.closing = Some(std::thread::spawn(move || {
                if e.capabilities().has_scope && e.is_running() {
                    let _ = e.request(Request::ScopeDataOutput(false));
                }
                e.shutdown();
                drop(sim);
                log.push("Disconnected");
            }));
        }
        self.sim = None;
    }

    /// A lost link on a real port (unplug, radio switched off) is retried every 3 s with the same
    /// settings until the operator presses Disconnect; the windows keep their state. A port that
    /// went away is looked up again by the radio's USB description, so a replug onto a different
    /// /dev name also reconnects.
    fn auto_reconnect(&mut self, ctx: &egui::Context) {
        if !self.settings.auto_reconnect || !self.want_link || self.connected() || !matches!(self.settings.port, PortChoice::Path(_)) {
            self.next_reconnect = None;
            return;
        }
        let now = Instant::now();
        let due = *self.next_reconnect.get_or_insert(now + Duration::from_secs(3));
        if now < due {
            return;
        }
        self.next_reconnect = Some(now + Duration::from_secs(3));
        if let PortChoice::Path(p) = &self.settings.port
            && !std::path::Path::new(p).exists()
        {
            match discovery::find(self.settings.rig) {
                Some(found) => self.settings.port = PortChoice::Path(found.path),
                None => return, // still unplugged
            }
        }
        self.connect(ctx);
    }

    fn act(&self, ctx: &egui::Context, what: &'static str, f: impl FnOnce(&Engine) -> Result<(), String> + Send + 'static) {
        let Some(e) = self.engine() else { return };
        let (log, ctx) = (self.log.clone(), ctx.clone());
        std::thread::spawn(move || {
            if let Err(err) = f(&e) {
                log.push(format!("{what}: {err}"));
            }
            ctx.request_repaint();
        });
    }

    /// Send a frequency to the radio. The display shows it at once (the radio confirms within a
    /// moment), so fast tuning shows every value instead of lagging behind the radio's reports.
    fn tune(&self, hz: u64) {
        if let Some(t) = &self.tuner {
            self.target.set(Some((hz, Instant::now())));
            let _ = t.send(hz);
        }
    }

    /// The frequency to show and to step from: the last one sent if it is recent, else the radio's.
    fn shown_frequency(&self) -> Option<u64> {
        let radio = self.engine().and_then(|e| e.state().frequency_hz);
        match self.target.get() {
            Some((hz, at)) if at.elapsed() < Duration::from_millis(1500) => Some(hz),
            _ => radio,
        }
    }

    fn apply_rigctld(&mut self) {
        self.rigctld = None;
        self.tci = None;
        self.rigctld_error = None;
        let allow: Vec<std::net::IpAddr> = self.settings.rigctld_allow.split([',', ' ']).filter_map(|a| a.trim().parse().ok()).collect();
        if self.settings.tci_enabled {
            let cfg = rigdeck_core::tci::TciConfig {
                port: self.settings.tci_port,
                lan: self.settings.rigctld_lan,
                allow: allow.clone(),
                allow_tx: self.settings.rigctld_allow_tx,
                tx_limit: Duration::from_secs(u64::from(self.settings.tx_limit_secs)),
            };
            match rigdeck_core::tci::TciServer::start(self.slot.clone(), cfg) {
                Ok(s) => {
                    self.log.push(format!("TCI listening on ws://{}", s.local_addr));
                    self.tci = Some(s);
                }
                Err(e) => {
                    self.log.push(format!("TCI: {e}"));
                    self.rigctld_error = Some(format!("TCI: {e}"));
                }
            }
        }
        if !self.settings.rigctld_enabled {
            return;
        }
        let cfg = RigctldConfig {
            port: self.settings.rigctld_port,
            lan: self.settings.rigctld_lan,
            allow,
            allow_tx: self.settings.rigctld_allow_tx,
            tx_limit: Duration::from_secs(u64::from(self.settings.tx_limit_secs)),
        };
        match RigctldServer::start(self.slot.clone(), cfg) {
            Ok(s) => {
                self.log.push(format!("rigctld listening on {}", s.local_addr));
                self.rigctld = Some(s);
            }
            Err(e) => {
                self.log.push(format!("rigctld: {e}"));
                self.rigctld_error = Some(e.to_string());
            }
        }
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.events else { return };
        let mut sweeps = Vec::new();
        while let Ok(e) = rx.try_recv() {
            if let Event::Sweep(s) = e {
                sweeps.push(s);
            }
        }
        for s in sweeps {
            self.sweep_times.push_back(Instant::now());
            self.waterfall.push(ctx, s, self.settings.display.smoothing);
        }
        while self.sweep_times.front().is_some_and(|t| t.elapsed() > Duration::from_secs(2)) {
            self.sweep_times.pop_front();
        }
    }

    fn automation(&mut self, ctx: &egui::Context) {
        let t = self.started.elapsed().as_secs_f32();
        if self.opts.read_memories && self.connected() && self.memories.rows.lock().unwrap().is_empty() && t > 1.0 {
            self.opts.read_memories = false;
            if let Some(e) = self.engine() {
                self.memories.read_all(e, ctx.clone());
            }
        }
        if let Some(n) = self.opts.tune_memory
            && t > 3.0
        {
            let ch = self.memories.rows.lock().unwrap().iter().find(|r| r.ch.number == n && !r.ch.empty).map(|r| r.ch.clone());
            if let Some(ch) = ch {
                self.opts.tune_memory = None;
                self.memories.tune_request = Some(ch);
            }
        }
        if self.opts.scroll_notches != 0 && t > 4.0 && self.connected() {
            let dir = self.opts.scroll_notches.signum();
            self.opts.scroll_notches -= dir;
            let steps = self.wheel_to_steps(Wheel { lines: dir as f32, shift: false, alt: false, command: false });
            self.step_tune(steps);
            println!("SHOWN {}", self.shown_frequency().unwrap_or(0));
            ctx.request_repaint();
        }
        if let Some(frac) = self.opts.click_at
            && t > 4.0
            && self.waterfall.last.is_some()
        {
            self.opts.click_at = None;
            if let Some(hz) = self.waterfall.click_frequency(frac, &self.settings.display, self.settings.step_hz, 0.012) {
                self.log.push(format!("Click at {:.0}% of the spectrum tunes to {hz} Hz", frac * 100.0));
                println!("CLICK_TUNE {hz}");
                self.tune(hz);
            }
        }
        if let Some(d) = self.opts.disconnect_after
            && t > d
            && !self.disconnected_by_option
        {
            if self.opts.cycles > 0 && self.cycles_done < self.opts.cycles {
                // Connect/disconnect cycling with the windows open.
                self.cycles_done += 1;
                self.disconnect();
                self.log.push(format!("cycle {}: disconnected", self.cycles_done));
                println!("cycle {}: disconnected", self.cycles_done);
                self.connect(ctx);
                self.opts.disconnect_after = Some(t + 1.0);
            } else {
                self.disconnected_by_option = true;
                self.want_link = false;
                self.disconnect();
            }
        }
        if let Some(path) = self.opts.screenshot.clone()
            && !self.screenshot_taken
            && t > self.opts.screenshot_after
        {
            self.screenshot_taken = true;
            let log = self.log.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                match save_png(&path, &img) {
                    Ok(()) => log.push(format!("Screenshot saved to {}", path.display())),
                    Err(e) => log.push(format!("Screenshot failed: {e}")),
                }
                println!("SCREENSHOT: {}", path.display());
            })));
        }
        if let Some(x) = self.opts.exit_after
            && t > x
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// Wait for the top of the minute, then set the radio's time and date to UTC and read it back.
pub fn sync_clock_utc(e: &Engine) -> Result<(), String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let wait = 60.0 - (now.as_secs() % 60) as f64 - f64::from(now.subsec_millis()) / 1000.0;
    std::thread::sleep(Duration::from_secs_f64(wait.max(0.0)));
    let out = std::process::Command::new("date").env("TZ", "UTC").arg("+%Y %m %d %H %M").output().map_err(|x| x.to_string())?;
    let p: Vec<u16> = String::from_utf8_lossy(&out.stdout).split_whitespace().filter_map(|v| v.parse().ok()).collect();
    let [y, mo, d, h, mi] = p[..] else { return Err("could not read the clock".into()) };
    e.request(Request::SetTime { hour: h as u8, minute: mi as u8 }).map_err(|x| x.to_string())?;
    e.request(Request::SetDate { year: y, month: mo as u8, day: d as u8 }).map_err(|x| x.to_string())?;
    match e.request(Request::ReadClockTime).map_err(|x| x.to_string())?.first_event() {
        Some(Event::ClockTime { hour, minute }) if u16::from(*hour) == h && u16::from(*minute) == mi => Ok(()),
        other => Err(format!("radio clock reads {other:?}")),
    }
}

fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), img.size[0] as u32, img.size[1] as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    w.write_image_data(&bytes).map_err(|e| e.to_string())
}

impl eframe::App for RigDeckApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        self.automation(ctx);
        self.auto_reconnect(ctx);
        // Redraw when the radio reports something (the waker thread started in connect()), and
        // at least every 500 ms for timers and countdowns. No fixed 20 fps loop.
        let busy = matches!(self.ptt, Ptt::Keyed { .. }) || {
            let j = self.memories.job.lock().unwrap();
            j.total > 0 && j.finished.not()
        };
        ctx.request_repaint_after(Duration::from_millis(if busy { 200 } else { 500 }));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if std::env::var_os("RIGDECK_FPS").is_some() {
            self.frames += 1;
            if self.fps_since.elapsed() >= Duration::from_secs(10) {
                println!("FPS {:.1}", self.frames as f64 / self.fps_since.elapsed().as_secs_f64());
                self.frames = 0;
                self.fps_since = Instant::now();
            }
        }
        self.layout(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "settings", &self.settings);
    }

    fn on_exit(&mut self) {
        // Stop scope output, unkey and release the port before the process ends.
        if let Some(e) = self.slot.write().unwrap().take() {
            if e.capabilities().has_scope && e.is_running() {
                let _ = e.request(Request::ScopeDataOutput(false));
            }
            let st = e.state().stats;
            println!(
                "STATS uptime_s={:.0} sweeps={} sweeps_dropped={} sent={} refused={} timeouts={}",
                self.started.elapsed().as_secs_f64(),
                st.sweeps,
                st.sweeps_dropped,
                st.sent,
                st.refused,
                st.timeouts
            );
            e.shutdown();
        }
        self.rigctld = None;
        self.tci = None;
    }
}
