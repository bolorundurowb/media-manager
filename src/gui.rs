//! egui desktop GUI (Phase 7).
//!
//! Pick a source root, optionally a destination root, assign its immediate
//! children to Movies or TV, and watch a streaming log while
//! [`media_manager::run_items`] does the work on a background thread. This
//! module never reimplements scan/parse/group/plan/exec — it only calls the
//! shared engine and renders the `LogEvent`s it sends back.
//!
//! Compiled only into the `media-manager-gui` binary, behind the `gui`
//! Cargo feature.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread::JoinHandle;

use eframe::egui;
use media_manager::{CancelToken, LibraryKind, LogEvent, WorkItem};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Assignment {
    Unassigned,
    Movies,
    Tv,
}

impl Assignment {
    fn label(self) -> &'static str {
        match self {
            Assignment::Unassigned => "unassigned",
            Assignment::Movies => "Movies",
            Assignment::Tv => "TV",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Idle,
    Scanning,
    Applying,
    Done,
    Failed,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Idle => "Idle",
            Status::Scanning => "Scanning",
            Status::Applying => "Applying",
            Status::Done => "Done",
            Status::Failed => "Failed",
        }
    }
}

struct RunHandle {
    receiver: Receiver<LogEvent>,
    cancel: CancelToken,
    join: Option<JoinHandle<()>>,
}

pub struct GuiApp {
    source: String,
    dest: String,
    children: Vec<PathBuf>,
    assignments: BTreeMap<PathBuf, Assignment>,
    selected: Vec<PathBuf>,
    apply: bool,
    status: Status,
    log: Vec<String>,
    run: Option<RunHandle>,
    last_summary: Option<(usize, usize, usize, usize, bool)>,
    error: Option<String>,
}

impl Default for GuiApp {
    fn default() -> Self {
        GuiApp {
            source: String::new(),
            dest: String::new(),
            children: Vec::new(),
            assignments: BTreeMap::new(),
            selected: Vec::new(),
            apply: false,
            status: Status::Idle,
            log: Vec::new(),
            run: None,
            last_summary: None,
            error: None,
        }
    }
}

impl GuiApp {
    fn refresh_children(&mut self) {
        self.children.clear();
        self.assignments.clear();
        self.selected.clear();
        self.error = None;

        let root = PathBuf::from(self.source.trim());
        if root.as_os_str().is_empty() {
            return;
        }
        if !root.is_dir() {
            self.error = Some(format!("not a directory: {}", root.display()));
            return;
        }
        let entries = match std::fs::read_dir(&root) {
            Ok(e) => e,
            Err(err) => {
                self.error = Some(format!("could not list {}: {err}", root.display()));
                return;
            }
        };
        let mut children: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        children.sort();
        for child in children {
            self.assignments
                .insert(child.clone(), Assignment::Unassigned);
            self.children.push(child);
        }
    }

    fn browse_source(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            self.source = path.display().to_string();
            self.refresh_children();
        }
    }

    fn browse_dest(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            self.dest = path.display().to_string();
        }
    }

    fn mark_selected(&mut self, assignment: Assignment) {
        for path in &self.selected {
            self.assignments.insert(path.clone(), assignment);
        }
    }

    fn can_start(&self) -> bool {
        self.run.is_none()
            && !self.source.trim().is_empty()
            && self
                .assignments
                .values()
                .any(|a| *a != Assignment::Unassigned)
    }

    fn start(&mut self) {
        let root = PathBuf::from(self.source.trim());
        let dest_text = self.dest.trim();
        let dest = if dest_text.is_empty() {
            None
        } else {
            Some(PathBuf::from(dest_text))
        };

        let items: Vec<WorkItem> = self
            .assignments
            .iter()
            .filter_map(|(path, assignment)| {
                let kind = match assignment {
                    Assignment::Movies => LibraryKind::Movies,
                    Assignment::Tv => LibraryKind::Tv,
                    Assignment::Unassigned => return None,
                };
                Some(WorkItem {
                    path: path.clone(),
                    kind,
                })
            })
            .collect();
        if items.is_empty() {
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = CancelToken::new();
        let cancel_for_thread = cancel.clone();
        let apply = self.apply;
        let join = std::thread::spawn(move || {
            let dest_ref = dest.as_deref();
            let _ = media_manager::run_items(&root, dest_ref, items, apply, &cancel_for_thread, tx);
        });

        self.log.clear();
        self.last_summary = None;
        self.error = None;
        self.status = Status::Scanning;
        self.run = Some(RunHandle {
            receiver: rx,
            cancel,
            join: Some(join),
        });
    }

    fn stop(&mut self) {
        if let Some(run) = &self.run {
            run.cancel.cancel();
        }
    }

    fn drain_events(&mut self) {
        let mut done = false;
        if let Some(run) = &mut self.run {
            loop {
                match run.receiver.try_recv() {
                    Ok(event) => {
                        match &event {
                            LogEvent::Finished {
                                moved,
                                merged,
                                skipped,
                                failed,
                                cancelled,
                            } => {
                                self.last_summary =
                                    Some((*moved, *merged, *skipped, *failed, *cancelled));
                                self.status = if *cancelled || *failed > 0 {
                                    Status::Failed
                                } else {
                                    Status::Done
                                };
                                done = true;
                            }
                            LogEvent::Scanning => {
                                self.status = Status::Scanning;
                            }
                            _ => {
                                self.status = Status::Applying;
                            }
                        }
                        self.log.push(format_event(&event));
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        done = true;
                        break;
                    }
                }
            }
        }
        if done {
            if let Some(mut run) = self.run.take() {
                if let Some(join) = run.join.take() {
                    let _ = join.join();
                }
            }
        }
    }
}

fn format_event(event: &LogEvent) -> String {
    match event {
        LogEvent::Scanning => "SCANNING".to_string(),
        LogEvent::CreateDir(path) => format!("CREATE  {}", path.display()),
        LogEvent::Moved { from, to } => format!("MOVE    {} -> {}", from.display(), to.display()),
        LogEvent::Skipped { path, reason } => format!("SKIP    {} ({reason})", path.display()),
        LogEvent::Failed { path, reason } => format!("FAIL    {} ({reason})", path.display()),
        LogEvent::Finished {
            moved,
            merged,
            skipped,
            failed,
            cancelled,
        } => format!(
            "SUMMARY moved={moved} merged={merged} skipped={skipped} failed={failed} cancelled={cancelled}"
        ),
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        if self.run.is_some() {
            ctx.request_repaint();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("media-manager");

            ui.horizontal(|ui| {
                ui.label("Source:");
                let response = ui.text_edit_singleline(&mut self.source);
                if response.lost_focus() {
                    self.refresh_children();
                }
                if ui.button("Browse…").clicked() {
                    self.browse_source();
                }
            });

            ui.horizontal(|ui| {
                ui.label("Dest (optional):");
                ui.text_edit_singleline(&mut self.dest);
                if ui.button("Browse…").clicked() {
                    self.browse_dest();
                }
            });
            ui.small("Leave dest empty to organise in place, same as the CLI.");

            if let Some(error) = &self.error {
                ui.colored_label(egui::Color32::RED, error.as_str());
            }

            ui.separator();
            ui.label("Children (select rows, then assign):");
            let children = self.children.clone();
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .id_salt("children")
                .show(ui, |ui| {
                    for child in &children {
                        let assignment = *self
                            .assignments
                            .get(child)
                            .unwrap_or(&Assignment::Unassigned);
                        let is_selected = self.selected.contains(child);
                        let name = child
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| child.display().to_string());
                        let text = format!("{name}  [{}]", assignment.label());
                        if ui.selectable_label(is_selected, text).clicked() {
                            if is_selected {
                                self.selected.retain(|p| p != child);
                            } else {
                                self.selected.push(child.clone());
                            }
                        }
                    }
                });

            ui.horizontal(|ui| {
                let has_selection = !self.selected.is_empty();
                if ui
                    .add_enabled(has_selection, egui::Button::new("Mark as Movies"))
                    .clicked()
                {
                    self.mark_selected(Assignment::Movies);
                }
                if ui
                    .add_enabled(has_selection, egui::Button::new("Mark as TV"))
                    .clicked()
                {
                    self.mark_selected(Assignment::Tv);
                }
                if ui
                    .add_enabled(has_selection, egui::Button::new("Clear"))
                    .clicked()
                {
                    self.mark_selected(Assignment::Unassigned);
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.apply, false, "Dry-run");
                ui.radio_value(&mut self.apply, true, "Apply");
            });

            ui.horizontal(|ui| {
                let running = self.run.is_some();
                if ui
                    .add_enabled(self.can_start() && !running, egui::Button::new("Start"))
                    .clicked()
                {
                    self.start();
                }
                if ui.add_enabled(running, egui::Button::new("Stop")).clicked() {
                    self.stop();
                }
                ui.label(format!("Status: {}", self.status.label()));
            });

            if let Some((moved, merged, skipped, failed, cancelled)) = self.last_summary {
                ui.label(format!(
                    "moved={moved} merged={merged} skipped={skipped} failed={failed} cancelled={cancelled}"
                ));
            }

            ui.separator();
            ui.label("Log:");
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .id_salt("log")
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.log {
                        ui.monospace(line);
                    }
                });
        });
    }
}
