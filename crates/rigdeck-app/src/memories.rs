//! Memory manager window: read all channels, edit in a table, CSV import with a preview of what
//! will change, automatic backup before any write, write with read-back verify per channel.

use crate::app::{Shared, backup_dir};
use eframe::egui;
use egui_extras::{Column, TableBuilder};
use rigdeck_core::Engine;
use rigdeck_core::memory::{self, ChangeKind, MemoryChannel};
use rigdeck_core::rig::{Mode, RigKind};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq)]
pub enum RowStatus {
    Read,
    Edited,
    Writing,
    Verified,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Row {
    pub ch: MemoryChannel,
    pub freq_text: String,
    pub status: RowStatus,
}

#[derive(Default)]
pub struct Job {
    pub label: String,
    pub done: usize,
    pub total: usize,
    pub finished: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct MemoryWindow {
    pub open: bool,
    pub rows: Arc<Mutex<Vec<Row>>>,
    pub job: Arc<Mutex<Job>>,
    pub preview: Option<Vec<memory::Change>>,
    pub message: String,
    /// Set when the operator clicks a channel to tune to it; the app consumes it.
    pub tune_request: Option<MemoryChannel>,
    /// The radio the rows were read from.
    pub kind: Option<RigKind>,
}

impl MemoryWindow {
    fn busy(&self) -> bool {
        let j = self.job.lock().unwrap();
        j.total > 0 && !j.finished
    }

    pub fn read_all(&mut self, engine: Arc<Engine>, ctx: egui::Context) {
        if self.busy() {
            return;
        }
        let range = engine.capabilities().memory_channels.clone();
        let total = range.clone().count();
        *self.job.lock().unwrap() = Job { label: "Reading channels".into(), total, ..Default::default() };
        let (rows, job) = (self.rows.clone(), self.job.clone());
        rows.lock().unwrap().clear();
        std::thread::spawn(move || {
            for n in range {
                match engine.read_memory(n) {
                    Ok(ch) => rows.lock().unwrap().push(Row { freq_text: freq_text(&ch), ch, status: RowStatus::Read }),
                    Err(e) => {
                        let mut j = job.lock().unwrap();
                        j.error = Some(format!("channel {n}: {e}"));
                        j.finished = true;
                        ctx.request_repaint();
                        return;
                    }
                }
                job.lock().unwrap().done += 1;
                ctx.request_repaint();
            }
            job.lock().unwrap().finished = true;
            ctx.request_repaint();
        });
    }

    /// Back up every channel, then write the given ones with read-back verify.
    pub fn write(&mut self, engine: Arc<Engine>, ctx: egui::Context, to_write: Vec<MemoryChannel>, log: Shared) {
        if self.busy() || to_write.is_empty() {
            return;
        }
        let kind = engine.capabilities().kind;
        let all_range = engine.capabilities().memory_channels.clone();
        *self.job.lock().unwrap() =
            Job { label: "Backing up all channels".into(), total: all_range.clone().count() + to_write.len(), ..Default::default() };
        let (rows, job) = (self.rows.clone(), self.job.clone());
        std::thread::spawn(move || {
            // 1. Backup.
            let mut backup = Vec::new();
            for n in all_range {
                match engine.read_memory(n) {
                    Ok(ch) => backup.push(ch),
                    Err(e) => {
                        let mut j = job.lock().unwrap();
                        j.error = Some(format!("backup stopped at channel {n}: {e}. Nothing was written."));
                        j.finished = true;
                        ctx.request_repaint();
                        return;
                    }
                }
                job.lock().unwrap().done += 1;
                ctx.request_repaint();
            }
            let path = backup_dir().join(format!("{}-memories-backup-{}.csv", file_tag(kind), crate::app::stamp()));
            if let Err(e) = std::fs::create_dir_all(backup_dir()).and_then(|_| std::fs::write(&path, memory::export_csv(kind, &backup))) {
                let mut j = job.lock().unwrap();
                j.error = Some(format!("could not save backup ({e}). Nothing was written."));
                j.finished = true;
                return;
            }
            log.push(format!("Backup of {} channels saved to {}", backup.len(), path.display()));
            job.lock().unwrap().label = "Writing and verifying".into();
            // 2. Write each channel, keeping settings we do not edit from what is in the radio.
            let mut failures = 0;
            for mut ch in to_write {
                if let Some(b) = backup.iter().find(|b| b.number == ch.number)
                    && !b.empty
                    && ch.extra == memory::MemoryExtra::None
                {
                    ch.extra = b.extra.clone();
                }
                set_status(&rows, ch.number, RowStatus::Writing);
                ctx.request_repaint();
                let st = match engine.write_memory(&ch) {
                    Ok(back) => {
                        let mut r = rows.lock().unwrap();
                        if let Some(row) = r.iter_mut().find(|r| r.ch.number == ch.number) {
                            row.ch = back.clone();
                            row.freq_text = freq_text(&back);
                        } else {
                            r.push(Row { freq_text: freq_text(&back), ch: back, status: RowStatus::Verified });
                            r.sort_by_key(|r| r.ch.number);
                        }
                        RowStatus::Verified
                    }
                    Err(e) => {
                        failures += 1;
                        RowStatus::Failed(e.to_string())
                    }
                };
                set_status(&rows, ch.number, st);
                job.lock().unwrap().done += 1;
                ctx.request_repaint();
            }
            let mut j = job.lock().unwrap();
            j.finished = true;
            if failures > 0 {
                j.error = Some(format!("{failures} channel(s) failed; see the Status column. Backup: {}", path.display()));
            }
            log.push(format!("Memory write finished: {failures} failure(s)"));
            ctx.request_repaint();
        });
    }

    pub fn show(&mut self, ctx: &egui::Context, engine: Option<Arc<Engine>>, log: &Shared) {
        let mut open = self.open;
        egui::Window::new("Memories").id(egui::Id::unique("memories_window")).open(&mut open).default_size([760.0, 520.0]).show(
            ctx,
            |ui| {
                let Some(engine) = engine.clone() else {
                    ui.label("Connect to a radio to manage its memories.");
                    return;
                };
                let kind = engine.capabilities().kind;
                let busy = self.busy();
                ui.horizontal(|ui| {
                    if ui.add_enabled(!busy, egui::Button::new("Read all")).clicked() {
                        self.read_all(engine.clone(), ctx.clone());
                    }
                    if ui.add_enabled(!busy, egui::Button::new("Import CSV…")).clicked()
                        && let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).pick_file()
                    {
                        match std::fs::read_to_string(&path)
                            .map_err(|e| e.to_string())
                            .and_then(|t| memory::import_csv(kind, &t).map_err(|e| e.to_string()))
                        {
                            Ok(planned) => {
                                let current: Vec<MemoryChannel> = self.rows.lock().unwrap().iter().map(|r| r.ch.clone()).collect();
                                self.preview = Some(memory::preview(&current, &planned));
                                self.message = format!("{} channels in {}", planned.len(), path.display());
                            }
                            Err(e) => self.message = format!("Import failed: {e}"),
                        }
                    }
                    if ui.add_enabled(!busy, egui::Button::new("Export CSV…")).clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("CSV", &["csv"])
                            .set_file_name(format!("{}-memories.csv", file_tag(kind)))
                            .save_file()
                    {
                        let chans: Vec<MemoryChannel> = self.rows.lock().unwrap().iter().map(|r| r.ch.clone()).collect();
                        self.message = match std::fs::write(&path, memory::export_csv(kind, &chans)) {
                            Ok(()) => format!("Exported {} channels to {}", chans.len(), path.display()),
                            Err(e) => format!("Export failed: {e}"),
                        };
                    }
                    let edited: Vec<MemoryChannel> =
                        self.rows.lock().unwrap().iter().filter(|r| r.status == RowStatus::Edited).map(|r| r.ch.clone()).collect();
                    if ui.add_enabled(!busy && !edited.is_empty(), egui::Button::new(format!("Write {} edited", edited.len()))).clicked() {
                        self.write(engine.clone(), ctx.clone(), edited, log.clone());
                    }
                });
                {
                    let j = self.job.lock().unwrap();
                    if j.total > 0 {
                        let frac = j.done as f32 / j.total as f32;
                        ui.add(egui::ProgressBar::new(frac).text(format!("{} {}/{}", j.label, j.done, j.total)));
                    }
                    if let Some(e) = &j.error {
                        ui.colored_label(egui::Color32::from_rgb(230, 90, 80), e);
                    }
                }
                if !self.message.is_empty() {
                    ui.label(&self.message);
                }
                ui.label(format!("Backups are saved to {} before every write.", backup_dir().display()));
                ui.separator();
                self.table(ui, kind, busy);
            },
        );
        self.open = open;
        self.preview_window(ctx, engine, log);
    }

    fn table(&mut self, ui: &mut egui::Ui, kind: RigKind, busy: bool) {
        let caps = kind.capabilities();
        let mut rows = self.rows.lock().unwrap();
        let mut tune: Option<MemoryChannel> = None;
        TableBuilder::new(ui)
            .striped(true)
            .column(Column::exact(30.0))
            .column(Column::exact(48.0))
            .column(Column::exact(110.0))
            .column(Column::exact(90.0))
            .column(Column::exact(50.0))
            .column(Column::exact(130.0))
            .column(Column::remainder())
            .header(22.0, |mut h| {
                for t in ["", "Ch", "Freq (MHz)", "Mode", "DATA", "Name", "Status"] {
                    h.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|body| {
                body.rows(24.0, rows.len(), |mut row| {
                    let r = &mut rows[row.index()];
                    let mut changed = false;
                    row.col(|ui| {
                        let b = ui
                            .add_enabled(!r.ch.empty, egui::Button::new("▶").small())
                            .on_hover_text("Tune the radio to this channel (frequency and mode)");
                        if b.clicked() {
                            tune = Some(r.ch.clone());
                        }
                    });
                    row.col(|ui| {
                        if ui.add(egui::Label::new(r.ch.label(kind)).sense(egui::Sense::click())).double_clicked() && !r.ch.empty {
                            tune = Some(r.ch.clone());
                        }
                    });
                    row.col(|ui| {
                        let resp = ui.add_enabled(!busy, egui::TextEdit::singleline(&mut r.freq_text).desired_width(100.0));
                        if resp.lost_focus() {
                            match r.freq_text.trim().parse::<f64>() {
                                Ok(mhz) if mhz > 0.0 => {
                                    let hz = (mhz * 1e6).round() as u64;
                                    if hz != r.ch.freq_hz || r.ch.empty {
                                        r.ch.freq_hz = hz;
                                        r.ch.empty = false;
                                        changed = true;
                                    }
                                }
                                Ok(_) | Err(_) if r.freq_text.trim().is_empty() => {}
                                _ => r.freq_text = freq_text(&r.ch),
                            }
                        }
                    });
                    row.col(|ui| {
                        ui.add_enabled_ui(!busy && !r.ch.empty, |ui| {
                            egui::ComboBox::from_id_salt(("mode", r.ch.number))
                                .selected_text(if r.ch.empty { "" } else { r.ch.mode.mode.label() })
                                .width(80.0)
                                .show_ui(ui, |ui| {
                                    for m in &caps.modes {
                                        if ui.selectable_label(r.ch.mode.mode == *m, m.label()).clicked() && r.ch.mode.mode != *m {
                                            r.ch.mode.mode = *m;
                                            changed = true;
                                        }
                                    }
                                });
                        });
                    });
                    row.col(|ui| {
                        let can_data = matches!(r.ch.mode.mode, Mode::Lsb | Mode::Usb | Mode::Fm | Mode::Am);
                        if ui.add_enabled(!busy && !r.ch.empty && can_data, egui::Checkbox::without_text(&mut r.ch.mode.data)).changed() {
                            changed = true;
                        }
                    });
                    row.col(|ui| {
                        let before = r.ch.name.clone();
                        ui.add_enabled(
                            !busy && !r.ch.empty,
                            egui::TextEdit::singleline(&mut r.ch.name).char_limit(caps.memory_name_len).desired_width(120.0),
                        );
                        if r.ch.name != before {
                            changed = true;
                        }
                    });
                    row.col(|ui| {
                        match &r.status {
                            RowStatus::Read => ui.label(if r.ch.empty { "empty" } else { "" }),
                            RowStatus::Edited => ui.colored_label(egui::Color32::from_rgb(240, 190, 60), "edited"),
                            RowStatus::Writing => ui.label("writing…"),
                            RowStatus::Verified => ui.colored_label(egui::Color32::from_rgb(90, 200, 110), "written, verified"),
                            RowStatus::Failed(e) => ui.colored_label(egui::Color32::from_rgb(230, 90, 80), e),
                        };
                    });
                    if changed {
                        r.status = RowStatus::Edited;
                    }
                });
            });
        drop(rows);
        if tune.is_some() {
            self.tune_request = tune;
        }
    }

    fn preview_window(&mut self, ctx: &egui::Context, engine: Option<Arc<Engine>>, log: &Shared) {
        let Some(changes) = &self.preview else { return };
        let mut keep = true;
        let mut write = false;
        egui::Window::new("Import preview").id(egui::Id::unique("import_preview")).collapsible(false).default_size([620.0, 420.0]).show(
            ctx,
            |ui| {
                let n = changes.iter().filter(|c| c.kind != ChangeKind::Unchanged).count();
                ui.label(format!("{n} of {} channels will change. A backup of every channel is saved first.", changes.len()));
                if self.rows.lock().unwrap().is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(240, 190, 60),
                        "Channels have not been read yet, so every row shows as new. Press Read all first for an exact preview.",
                    );
                }
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    egui::Grid::new("preview_grid").striped(true).show(ui, |ui| {
                        ui.strong("Ch");
                        ui.strong("Change");
                        ui.strong("Now");
                        ui.strong("After");
                        ui.end_row();
                        for c in changes {
                            ui.label(c.number.to_string());
                            ui.label(format!("{:?}", c.kind));
                            ui.label(c.before.as_ref().map(describe).unwrap_or_else(|| "—".into()));
                            ui.label(describe(&c.after));
                            ui.end_row();
                        }
                    });
                });
                ui.horizontal(|ui| {
                    if ui.add_enabled(n > 0 && engine.is_some(), egui::Button::new(format!("Back up and write {n} channels"))).clicked() {
                        write = true;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
            },
        );
        if write {
            let to_write: Vec<MemoryChannel> =
                changes.iter().filter(|c| c.kind != ChangeKind::Unchanged).map(|c| c.after.clone()).collect();
            if let Some(e) = engine {
                self.write(e, ctx.clone(), to_write, log.clone());
            }
            keep = false;
        }
        if !keep {
            self.preview = None;
        }
    }
}

fn set_status(rows: &Arc<Mutex<Vec<Row>>>, n: u16, st: RowStatus) {
    if let Some(r) = rows.lock().unwrap().iter_mut().find(|r| r.ch.number == n) {
        r.status = st;
    }
}

pub fn freq_text(ch: &MemoryChannel) -> String {
    if ch.empty { String::new() } else { format!("{:.6}", ch.freq_hz as f64 / 1e6) }
}

fn describe(ch: &MemoryChannel) -> String {
    if ch.empty { "empty".into() } else { format!("{:.6} {} {}", ch.freq_hz as f64 / 1e6, ch.mode.label(), ch.name) }
}

pub fn file_tag(kind: RigKind) -> &'static str {
    match kind {
        RigKind::IcomIc7300 => "ic7300",
        RigKind::YaesuFt991a => "ft991a",
    }
}
