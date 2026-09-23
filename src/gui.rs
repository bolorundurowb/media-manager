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
    Planning,
    Applying,
    Done,
    Cancelled,
    Failed,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Idle => "Idle",
            Status::Scanning => "Scanning",
            Status::Planning => "Planning",
            Status::Applying => "Applying",
            Status::Done => "Done",
            Status::Cancelled => "Cancelled",
            Status::Failed => "Failed",
        }
    }

    fn is_running(self) -> bool {
        matches!(self, Status::Scanning | Status::Planning | Status::Applying)
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
    fn assignment_counts(&self) -> (usize, usize, usize) {
        self.assignments
            .values()
            .fold(
                (0, 0, 0),
                |(movies, tv, unassigned), assignment| match assignment {
                    Assignment::Movies => (movies + 1, tv, unassigned),
                    Assignment::Tv => (movies, tv + 1, unassigned),
                    Assignment::Unassigned => (movies, tv, unassigned + 1),
                },
            )
    }

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
            && self.input_error().is_none()
            && self
                .assignments
                .values()
                .any(|a| *a != Assignment::Unassigned)
    }

    fn input_error(&self) -> Option<String> {
        let root = PathBuf::from(self.source.trim());
        if !root.is_dir() {
            return Some(format!("not a directory: {}", root.display()));
        }
        let text = self.dest.trim();
        if text.is_empty() {
            return None;
        }
        let dest = PathBuf::from(text);
        if dest.exists() && !dest.is_dir() {
            return Some(format!(
                "destination is not a directory: {}",
                dest.display()
            ));
        }
        if !dest.exists() {
            let mut ancestor = dest.parent();
            while let Some(path) = ancestor {
                if path.exists() {
                    if !path.is_dir() {
                        return Some(format!(
                            "destination parent is not a directory: {}",
                            path.display()
                        ));
                    }
                    return None;
                }
                ancestor = path.parent();
            }
            return Some(format!(
                "destination has no existing parent: {}",
                dest.display()
            ));
        }
        None
    }

    fn start(&mut self) {
        if let Some(err) = self.input_error() {
            self.error = Some(err);
            return;
        }
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
                                self.status = if *cancelled {
                                    Status::Cancelled
                                } else if *failed > 0 {
                                    Status::Failed
                                } else {
                                    Status::Done
                                };
                                done = true;
                            }
                            LogEvent::Scanning => {
                                self.status = Status::Scanning;
                            }
                            LogEvent::CreateDir(_) | LogEvent::PlannedMove { .. }
                                if !self.apply =>
                            {
                                self.status = Status::Planning;
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
        LogEvent::JobStarted(path) => format!("JOB     {} (started)", path.display()),
        LogEvent::JobFinished(path) => format!("JOB     {} (finished)", path.display()),
        LogEvent::CreateDir(path) => format!("CREATE  {}", path.display()),
        LogEvent::PlannedMove { from, to } => {
            format!("MOVE    {} -> {}", from.display(), to.display())
        }
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
            "Summary: {moved} moved, {merged} merged, {skipped} skipped, \
             {failed} failed, cancelled={cancelled}"
        ),
    }
}

fn status_color(status: Status, visuals: &egui::Visuals) -> egui::Color32 {
    match status {
        Status::Done => {
            if visuals.dark_mode {
                egui::Color32::LIGHT_GREEN
            } else {
                egui::Color32::DARK_GREEN
            }
        }
        Status::Cancelled => visuals.warn_fg_color,
        Status::Failed => visuals.error_fg_color,
        Status::Scanning | Status::Planning | Status::Applying => visuals.hyperlink_color,
        Status::Idle => visuals.weak_text_color(),
    }
}

fn assignment_color(assignment: Assignment, visuals: &egui::Visuals) -> egui::Color32 {
    match assignment {
        Assignment::Movies => visuals.hyperlink_color,
        Assignment::Tv => {
            if visuals.dark_mode {
                egui::Color32::LIGHT_GREEN
            } else {
                egui::Color32::DARK_GREEN
            }
        }
        Assignment::Unassigned => visuals.weak_text_color(),
    }
}

impl eframe::App for GuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        if self.run.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }

        let running = self.run.is_some();
        egui::Frame::central_panel(ui.style())
            .inner_margin(egui::Margin::same(18))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.heading("Media Manager");
                        ui.label(
                            egui::RichText::new("Organise media safely for Jellyfin")
                                .color(ui.visuals().weak_text_color()),
                        );
                    });
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            egui::widgets::global_theme_preference_buttons(ui);
                        },
                    );
                });

                ui.add_space(14.0);
                egui::Frame::group(ui.style())
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.strong("1  Choose folders");
                        ui.add_space(8.0);

                        egui::Grid::new("folder_inputs")
                            .num_columns(3)
                            .spacing([10.0, 8.0])
                            .show(ui, |ui| {
                                ui.label("Source");
                                let source_response = ui.add_enabled(
                                    !running,
                                    egui::TextEdit::singleline(&mut self.source)
                                        .desired_width(f32::INFINITY)
                                        .hint_text("Folder containing your media"),
                                );
                                if !running
                                    && source_response.lost_focus()
                                    && ui.input(|input| input.key_pressed(egui::Key::Enter))
                                {
                                    self.refresh_children();
                                }
                                if ui
                                    .add_enabled(!running, egui::Button::new("Browse…"))
                                    .clicked()
                                {
                                    self.browse_source();
                                }
                                ui.end_row();

                                ui.label("Destination");
                                ui.add_enabled(
                                    !running,
                                    egui::TextEdit::singleline(&mut self.dest)
                                        .desired_width(f32::INFINITY)
                                        .hint_text("Optional — leave empty to organise in place"),
                                );
                                if ui
                                    .add_enabled(!running, egui::Button::new("Browse…"))
                                    .clicked()
                                {
                                    self.browse_dest();
                                }
                                ui.end_row();
                            });

                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(
                                "Only assigned items are scanned. An empty destination keeps files in the source.",
                            )
                            .small()
                            .color(ui.visuals().weak_text_color()),
                        );

                        if let Some(error) = &self.error {
                            ui.add_space(6.0);
                            ui.colored_label(ui.visuals().error_fg_color, format!("⚠  {error}"));
                        }
                    });

                ui.add_space(10.0);
                egui::Frame::group(ui.style())
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        let (movies, tv, unassigned) = self.assignment_counts();
                        ui.horizontal(|ui| {
                            ui.strong("2  Assign library types");
                            ui.separator();
                            ui.label(format!(
                                "{movies} movies  •  {tv} TV  •  {unassigned} unassigned"
                            ));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_enabled(
                                            !running && !self.children.is_empty(),
                                            egui::Button::new("Select all").small(),
                                        )
                                        .clicked()
                                    {
                                        self.selected = self.children.clone();
                                    }
                                    if ui
                                        .add_enabled(
                                            !running && !self.selected.is_empty(),
                                            egui::Button::new("Deselect").small(),
                                        )
                                        .clicked()
                                    {
                                        self.selected.clear();
                                    }
                                },
                            );
                        });
                        ui.add_space(6.0);

                        let children = self.children.clone();
                        egui::Frame::new()
                            .fill(ui.visuals().extreme_bg_color)
                            .inner_margin(6.0)
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .max_height(210.0)
                                    .id_salt("children")
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        if children.is_empty() {
                                            ui.centered_and_justified(|ui| {
                                                ui.label(
                                                    egui::RichText::new(
                                                        "Choose a source folder to see its contents",
                                                    )
                                                    .color(ui.visuals().weak_text_color()),
                                                );
                                            });
                                        }
                                        for child in &children {
                                            let assignment = *self
                                                .assignments
                                                .get(child)
                                                .unwrap_or(&Assignment::Unassigned);
                                            let is_selected = self.selected.contains(child);
                                            let name = child
                                                .file_name()
                                                .map(|name| name.to_string_lossy().to_string())
                                                .unwrap_or_else(|| child.display().to_string());

                                            ui.horizontal(|ui| {
                                                let available =
                                                    (ui.available_width() - 110.0).max(120.0);
                                                let response = ui.add_enabled(
                                                    !running,
                                                    egui::Button::selectable(
                                                        is_selected,
                                                        name,
                                                    )
                                                    .min_size(egui::vec2(available, 26.0)),
                                                );
                                                response.clone().on_hover_text(child.display().to_string());
                                                ui.label(
                                                    egui::RichText::new(assignment.label())
                                                        .strong()
                                                        .color(assignment_color(
                                                            assignment,
                                                            ui.visuals(),
                                                        )),
                                                );
                                                if response.clicked() {
                                                    if is_selected {
                                                        self.selected.retain(|path| path != child);
                                                    } else {
                                                        self.selected.push(child.clone());
                                                    }
                                                }
                                            });
                                        }
                                    });
                            });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(format!("{} selected", self.selected.len()));
                            let has_selection = !self.selected.is_empty() && !running;
                            if ui
                                .add_enabled(
                                    has_selection,
                                    egui::Button::new("🎬  Mark as Movies"),
                                )
                                .clicked()
                            {
                                self.mark_selected(Assignment::Movies);
                            }
                            if ui
                                .add_enabled(has_selection, egui::Button::new("📺  Mark as TV"))
                                .clicked()
                            {
                                self.mark_selected(Assignment::Tv);
                            }
                            if ui
                                .add_enabled(has_selection, egui::Button::new("Clear assignment"))
                                .clicked()
                            {
                                self.mark_selected(Assignment::Unassigned);
                            }
                        });
                    });

                ui.add_space(10.0);
                egui::Frame::group(ui.style())
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.strong("3  Review and run");
                            ui.add_enabled_ui(!running, |ui| {
                                ui.radio_value(&mut self.apply, false, "Preview only");
                                ui.radio_value(&mut self.apply, true, "Apply changes");
                            });

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_enabled(
                                            running,
                                            egui::Button::new("Stop").fill(
                                                ui.visuals().widgets.inactive.weak_bg_fill,
                                            ),
                                        )
                                        .clicked()
                                    {
                                        self.stop();
                                    }
                                    let start_label = if self.apply {
                                        "Apply changes"
                                    } else {
                                        "Preview changes"
                                    };
                                    if ui
                                        .add_enabled(
                                            self.can_start() && !running,
                                            egui::Button::new(start_label)
                                                .fill(ui.visuals().selection.bg_fill),
                                        )
                                        .clicked()
                                    {
                                        self.start();
                                    }
                                },
                            );
                        });

                        if self.apply && !running {
                            ui.label(
                                egui::RichText::new(
                                    "Apply mode moves files. Existing destinations are never overwritten.",
                                )
                                .small()
                                .color(ui.visuals().warn_fg_color),
                            );
                        }

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if self.status.is_running() {
                                ui.spinner();
                            }
                            ui.label("Status");
                            ui.label(
                                egui::RichText::new(self.status.label())
                                    .strong()
                                    .color(status_color(self.status, ui.visuals())),
                            );

                            if let Some((moved, merged, skipped, failed, cancelled)) =
                                self.last_summary
                            {
                                ui.separator();
                                ui.label(format!(
                                    "{moved} moved  •  {merged} merged  •  {skipped} skipped  •  {failed} failed"
                                ));
                                if cancelled {
                                    ui.label(
                                        egui::RichText::new("cancelled")
                                            .color(ui.visuals().warn_fg_color),
                                    );
                                }
                            }
                        });
                    });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.strong("Activity log");
                    ui.label(
                        egui::RichText::new(format!("{} events", self.log.len()))
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui
                                .add_enabled(
                                    !running && !self.log.is_empty(),
                                    egui::Button::new("Clear log").small(),
                                )
                                .clicked()
                            {
                                self.log.clear();
                                self.last_summary = None;
                            }
                        },
                    );
                });
                egui::Frame::new()
                    .fill(ui.visuals().code_bg_color)
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(ui.available_height().max(120.0))
                            .id_salt("log")
                            .stick_to_bottom(true)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if self.log.is_empty() {
                                    ui.label(
                                        egui::RichText::new(
                                            "Run a preview to inspect planned changes here.",
                                        )
                                        .monospace()
                                        .color(ui.visuals().weak_text_color()),
                                    );
                                }
                                for line in &self.log {
                                    ui.label(egui::RichText::new(line).monospace());
                                }
                            });
                    });
            });
    }
}
