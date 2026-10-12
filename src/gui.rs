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

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread::JoinHandle;

use eframe::egui;
use media_manager::{CancelToken, LibraryKind, LogEvent, WorkItem};

#[path = "gui/theme.rs"]
mod theme;
#[path = "gui/widgets.rs"]
mod widgets;

use theme::{bold_font, Palette};
use widgets::{Btn, BtnStyle, Icon};

const ROW_HEIGHT: f32 = 42.0;
/// The page scrolls as a whole, so the log gets a fixed height.
const LOG_HEIGHT: f32 = 240.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Assignment {
    Unassigned,
    Movies,
    Tv,
}

impl Assignment {
    /// Text of the badge shown at the end of a list row.
    fn badge(self) -> &'static str {
        match self {
            Assignment::Unassigned => "Unassigned",
            Assignment::Movies => "Movies",
            Assignment::Tv => "TV Shows",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Assignment::Unassigned => Icon::Folder,
            Assignment::Movies => Icon::Clapper,
            Assignment::Tv => Icon::Monitor,
        }
    }

    fn badge_colors(self, p: &Palette) -> (egui::Color32, egui::Color32) {
        match self {
            Assignment::Unassigned => (p.weak, p.button),
            Assignment::Movies => (p.movies_fg, p.movies_bg),
            Assignment::Tv => (p.tv_fg, p.tv_bg),
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
    selected: HashSet<PathBuf>,
    apply: bool,
    status: Status,
    log: Vec<String>,
    run: Option<RunHandle>,
    last_summary: Option<(usize, usize, usize, usize, bool)>,
    error: Option<String>,
    /// Spare window height shared by the file list and the log.
    extra_height: f32,
}

impl Default for GuiApp {
    fn default() -> Self {
        GuiApp {
            source: String::new(),
            dest: String::new(),
            children: Vec::new(),
            assignments: BTreeMap::new(),
            selected: HashSet::new(),
            apply: false,
            status: Status::Idle,
            log: Vec::new(),
            run: None,
            last_summary: None,
            error: None,
            extra_height: 0.0,
        }
    }
}

impl GuiApp {
    /// Builds the app and installs Manrope, the palettes and the text styles.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        GuiApp::default()
    }

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

fn status_color(status: Status, p: &Palette, visuals: &egui::Visuals) -> egui::Color32 {
    match status {
        Status::Done => p.ok,
        Status::Cancelled => visuals.warn_fg_color,
        Status::Failed => visuals.error_fg_color,
        Status::Idle | Status::Scanning | Status::Planning | Status::Applying => p.accent,
    }
}

fn log_line_color(line: &str, p: &Palette, visuals: &egui::Visuals) -> egui::Color32 {
    if line.starts_with("FAIL") {
        visuals.error_fg_color
    } else if line.starts_with("SKIP") {
        visuals.warn_fg_color
    } else if line.starts_with("Summary") {
        p.accent
    } else {
        p.text
    }
}

impl GuiApp {
    fn folders_card(&mut self, ui: &mut egui::Ui, running: bool) {
        let p = Palette::of(ui.visuals());
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                widgets::section_title(ui, 1, "Choose folders");
                widgets::right(ui, |ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                    widgets::paint_icon(ui.painter(), rect, Icon::Folder, p.weak);
                });
            });
            ui.add_space(8.0);

            let gap = 12.0;
            let source_label = 62.0;
            let dest_label = 86.0;
            let row_height = 38.0;
            let group_width =
                ((ui.available_width() - source_label - dest_label - gap * 3.0) / 2.0).max(150.0);
            let mut browse_source = false;
            let mut browse_dest = false;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.allocate_ui(egui::vec2(source_label, row_height), |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new("Source").color(p.weak))
                    });
                });
                let (response, browse) = ui
                    .allocate_ui(egui::vec2(group_width, row_height), |ui| {
                        widgets::path_input(
                            ui,
                            group_width,
                            &mut self.source,
                            !running,
                            "Folder containing your media",
                        )
                    })
                    .inner;
                browse_source = browse;
                if !running
                    && response.lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter))
                {
                    self.refresh_children();
                }
                ui.allocate_ui(egui::vec2(dest_label, row_height), |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new("Destination").color(p.weak))
                    });
                });
                let (_, browse) = ui
                    .allocate_ui(egui::vec2(group_width, row_height), |ui| {
                        widgets::path_input(
                            ui,
                            group_width,
                            &mut self.dest,
                            !running,
                            "Optional — leave empty to organise in place",
                        )
                    })
                    .inner;
                browse_dest = browse;
            });
            if browse_source {
                self.browse_source();
            }
            if browse_dest {
                self.browse_dest();
            }

            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Only assigned items are scanned. An empty destination keeps files in the source.",
                )
                .color(p.weak),
            );

            if let Some(error) = &self.error {
                ui.add_space(6.0);
                ui.colored_label(ui.visuals().error_fg_color, format!("⚠  {error}"));
            }
        });
    }

    fn assign_card(&mut self, ui: &mut egui::Ui, running: bool) {
        let p = Palette::of(ui.visuals());
        widgets::card(ui, |ui| {
            let (movies, tv, unassigned) = self.assignment_counts();
            ui.horizontal(|ui| {
                widgets::section_title(ui, 2, "Assign library types");
                ui.add_space(2.0);
                ui.label(egui::RichText::new(format!("{movies} movies")).color(p.accent));
                ui.label(
                    egui::RichText::new(format!("•  {tv} TV  •  {unassigned} unassigned"))
                        .color(p.weak),
                );
                widgets::right(ui, |ui| {
                    // Added right-to-left, so this reads Deselect, Refresh, Select all.
                    if Btn::new("Select all", BtnStyle::Secondary)
                        .small()
                        .enabled(!running && !self.children.is_empty())
                        .show(ui)
                        .clicked()
                    {
                        self.selected = self.children.iter().cloned().collect();
                    }
                    if Btn::new("Refresh", BtnStyle::Secondary)
                        .small()
                        .icon(Icon::Refresh)
                        .enabled(!running)
                        .show(ui)
                        .clicked()
                    {
                        self.refresh_children();
                    }
                    if Btn::new("Deselect", BtnStyle::Secondary)
                        .small()
                        .enabled(!running && !self.selected.is_empty())
                        .show(ui)
                        .clicked()
                    {
                        self.selected.clear();
                    }
                });
            });
            ui.add_space(10.0);

            let mut toggled: Option<PathBuf> = None;
            egui::Frame::new()
                .fill(p.input)
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(0, 4))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .max_height(ROW_HEIGHT * 5.0 + self.extra_height / 2.0)
                        .id_salt("children")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            if self.children.is_empty() {
                                ui.add_space(24.0);
                                ui.vertical_centered(|ui| {
                                    ui.label(
                                        egui::RichText::new(
                                            "Choose a source folder to see its contents",
                                        )
                                        .color(p.weak),
                                    );
                                });
                            }
                            for child in &self.children {
                                let assignment = *self
                                    .assignments
                                    .get(child)
                                    .unwrap_or(&Assignment::Unassigned);
                                let checked = self.selected.contains(child);
                                if assignment_row(ui, child, assignment, checked, !running, &p) {
                                    toggled = Some(child.clone());
                                }
                            }
                        });
                });
            if let Some(path) = toggled {
                if !self.selected.remove(&path) {
                    self.selected.insert(path);
                }
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{} selected", self.selected.len()))
                        .font(bold_font(14.0)),
                );
                widgets::right(ui, |ui| {
                    let has_selection = !self.selected.is_empty() && !running;
                    if Btn::new("Clear assignment", BtnStyle::Secondary)
                        .enabled(has_selection)
                        .show(ui)
                        .clicked()
                    {
                        self.mark_selected(Assignment::Unassigned);
                    }
                    if Btn::new("Mark as TV", BtnStyle::Secondary)
                        .icon(Icon::Monitor)
                        .enabled(has_selection)
                        .show(ui)
                        .clicked()
                    {
                        self.mark_selected(Assignment::Tv);
                    }
                    if Btn::new("Mark as Movies", BtnStyle::Secondary)
                        .icon(Icon::Clapper)
                        .enabled(has_selection)
                        .show(ui)
                        .clicked()
                    {
                        self.mark_selected(Assignment::Movies);
                    }
                });
            });
        });
    }

    fn run_card(&mut self, ui: &mut egui::Ui, running: bool) {
        let p = Palette::of(ui.visuals());
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                widgets::section_title(ui, 3, "Review and run");
                ui.add_space(4.0);
                if widgets::radio(ui, !self.apply, "Preview only", !running).clicked() {
                    self.apply = false;
                }
                if widgets::radio(ui, self.apply, "Apply changes", !running).clicked() {
                    self.apply = true;
                }

                widgets::right(ui, |ui| {
                    // Right-to-left: Stop ends up to the right of the primary button.
                    if Btn::new("Stop", BtnStyle::Secondary)
                        .enabled(running)
                        .min_width(64.0)
                        .show(ui)
                        .clicked()
                    {
                        self.stop();
                    }
                    let start_label = if self.apply {
                        "Apply changes"
                    } else {
                        "Preview changes"
                    };
                    if Btn::new(start_label, BtnStyle::Primary)
                        .icon(Icon::Play)
                        .enabled(self.can_start() && !running)
                        .show(ui)
                        .clicked()
                    {
                        self.start();
                    }
                });
            });

            if self.apply && !running {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Apply mode moves files. Existing destinations are never overwritten.",
                    )
                    .small()
                    .color(ui.visuals().warn_fg_color),
                );
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if self.status.is_running() {
                    ui.spinner();
                }
                ui.label(egui::RichText::new("Status").color(p.weak));
                ui.label(
                    egui::RichText::new(self.status.label())
                        .font(bold_font(14.0))
                        .color(status_color(self.status, &p, ui.visuals())),
                );

                if let Some((moved, merged, skipped, failed, cancelled)) = self.last_summary {
                    ui.separator();
                    ui.label(
                        egui::RichText::new(format!(
                            "{moved} moved  •  {merged} merged  •  {skipped} skipped  •  {failed} failed"
                        ))
                        .color(p.weak),
                    );
                    if cancelled {
                        ui.label(
                            egui::RichText::new("cancelled").color(ui.visuals().warn_fg_color),
                        );
                    }
                }
            });
        });
    }

    fn log_card(&mut self, ui: &mut egui::Ui, running: bool) {
        let p = Palette::of(ui.visuals());
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Activity log").font(bold_font(16.0)));
                widgets::pill(ui, &format!("{} events", self.log.len()), p.weak, p.button);
                widgets::right(ui, |ui| {
                    if Btn::new("Clear log", BtnStyle::Link)
                        .small()
                        .enabled(!running && !self.log.is_empty())
                        .show(ui)
                        .clicked()
                    {
                        self.log.clear();
                        self.last_summary = None;
                    }
                });
            });
            ui.add_space(8.0);
            egui::Frame::new()
                .fill(p.input)
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::same(14))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .max_height(LOG_HEIGHT + self.extra_height / 2.0)
                        .id_salt("log")
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if self.log.is_empty() {
                                ui.label(
                                    egui::RichText::new(
                                        "Run a preview to inspect planned changes here.",
                                    )
                                    .small()
                                    .italics()
                                    .color(p.weak),
                                );
                            }
                            for line in &self.log {
                                ui.label(
                                    egui::RichText::new(line).monospace().color(log_line_color(
                                        line,
                                        &p,
                                        ui.visuals(),
                                    )),
                                );
                            }
                        });
                });
        });
    }
}

/// One list row: checkbox, type icon, full path and a type badge. The whole
/// row toggles selection. Returns true when it was clicked.
fn assignment_row(
    ui: &mut egui::Ui,
    path: &std::path::Path,
    assignment: Assignment,
    checked: bool,
    enabled: bool,
    p: &Palette,
) -> bool {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), sense);
    if !ui.is_rect_visible(rect) {
        return response.clicked();
    }
    if enabled && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let painter = ui.painter();
    if enabled && response.hovered() {
        painter.rect_filled(rect, 0.0, p.button.gamma_multiply(0.5));
    }
    painter.hline(
        rect.x_range().shrink(14.0),
        rect.bottom(),
        egui::Stroke::new(1.0, p.divider),
    );

    let cy = rect.center().y;
    let checkbox = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 14.0 + 9.0, cy),
        egui::vec2(18.0, 18.0),
    );
    widgets::paint_checkbox(painter, checkbox, checked, enabled, p);

    let icon_rect =
        egui::Rect::from_center_size(egui::pos2(rect.left() + 56.0, cy), egui::vec2(16.0, 16.0));
    widgets::paint_icon(painter, icon_rect, assignment.icon(), p.accent);

    let (fg, bg) = assignment.badge_colors(p);
    let pill_size = widgets::pill_size(painter, assignment.badge(), 12.0);
    let pill_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - 14.0 - pill_size.x, cy - pill_size.y / 2.0),
        pill_size,
    );
    widgets::paint_pill(painter, pill_rect, assignment.badge(), 12.0, fg, bg, true);

    let text_x = rect.left() + 76.0;
    let max_width = (pill_rect.left() - 14.0 - text_x).max(40.0);
    let mut job = egui::text::LayoutJob::simple_singleline(
        path.display().to_string(),
        egui::FontId::proportional(14.0),
        p.text,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(text_x, cy - galley.size().y / 2.0),
        galley,
        p.text,
    );

    let response = response.on_hover_text(path.display().to_string());
    response.clicked()
}

impl eframe::App for GuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        if self.run.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }

        let running = self.run.is_some();
        // The panel frame fills the whole window (no stray background); the page
        // scrolls inside it. Spare vertical space is split between the file list
        // and the log.
        egui::Frame::central_panel(ui.style())
            .inner_margin(egui::Margin::ZERO)
            .show(ui, |ui| {
                let viewport_height = ui.available_height();
                let mut content_height = 0.0;
                egui::ScrollArea::vertical()
                    .id_salt("page")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let response = egui::Frame::new()
                            .inner_margin(egui::Margin::same(20))
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 14.0;
                                ui.horizontal(|ui| {
                                    widgets::right(ui, widgets::theme_switcher);
                                });
                                self.folders_card(ui, running);
                                self.assign_card(ui, running);
                                self.run_card(ui, running);
                                self.log_card(ui, running);
                            });
                        content_height = response.response.rect.height();
                    });

                // Height the page needs with no extra space handed out.
                let base = content_height - self.extra_height;
                let extra = (viewport_height - base - 1.0).max(0.0);
                if (extra - self.extra_height).abs() > 0.5 {
                    self.extra_height = extra;
                    ui.ctx().request_repaint();
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    /// Self-cleaning scratch directory (no dev-dependency needed).
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "media-manager-gui-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn app_with_source(dir: &Path) -> GuiApp {
        let mut app = GuiApp {
            source: dir.display().to_string(),
            ..GuiApp::default()
        };
        app.refresh_children();
        app
    }

    #[test]
    fn status_labels_and_running_flag() {
        let all = [
            (Status::Idle, "Idle", false),
            (Status::Scanning, "Scanning", true),
            (Status::Planning, "Planning", true),
            (Status::Applying, "Applying", true),
            (Status::Done, "Done", false),
            (Status::Cancelled, "Cancelled", false),
            (Status::Failed, "Failed", false),
        ];
        for (status, label, running) in all {
            assert_eq!(status.label(), label);
            assert_eq!(status.is_running(), running);
        }
    }

    #[test]
    fn assignment_badges_and_icons() {
        assert_eq!(Assignment::Movies.badge(), "Movies");
        assert_eq!(Assignment::Tv.badge(), "TV Shows");
        assert_eq!(Assignment::Unassigned.badge(), "Unassigned");
        assert_eq!(Assignment::Movies.icon(), Icon::Clapper);
        assert_eq!(Assignment::Tv.icon(), Icon::Monitor);
        assert_eq!(Assignment::Unassigned.icon(), Icon::Folder);
        for assignment in [Assignment::Movies, Assignment::Tv, Assignment::Unassigned] {
            for palette in [Palette::LIGHT, Palette::DARK] {
                let (fg, bg) = assignment.badge_colors(&palette);
                assert_ne!(fg, bg);
            }
        }
    }

    #[test]
    fn palette_follows_visuals() {
        assert!(Palette::of(&egui::Visuals::dark()).dark);
        assert!(!Palette::of(&egui::Visuals::light()).dark);
    }

    #[test]
    fn refresh_lists_sorted_children() {
        let scratch = Scratch::new();
        std::fs::create_dir(scratch.path().join("b_show")).unwrap();
        std::fs::create_dir(scratch.path().join("a_movie")).unwrap();
        let app = app_with_source(scratch.path());

        assert_eq!(app.children.len(), 2);
        assert!(app.children[0].ends_with("a_movie"));
        assert!(app.children[1].ends_with("b_show"));
        assert_eq!(app.assignment_counts(), (0, 0, 2));
        assert!(app.error.is_none());
    }

    #[test]
    fn refresh_reports_bad_source_and_ignores_empty() {
        let mut app = GuiApp::default();
        app.refresh_children();
        assert!(app.children.is_empty());
        assert!(app.error.is_none());

        app.source = "definitely/not/a/real/dir".to_string();
        app.refresh_children();
        assert!(app.error.as_deref().unwrap().contains("not a directory"));
    }

    #[test]
    fn marking_selection_updates_counts() {
        let scratch = Scratch::new();
        for name in ["one", "two", "three"] {
            std::fs::create_dir(scratch.path().join(name)).unwrap();
        }
        let mut app = app_with_source(scratch.path());
        app.selected = app.children.iter().take(2).cloned().collect();

        app.mark_selected(Assignment::Movies);
        assert_eq!(app.assignment_counts(), (2, 0, 1));
        app.mark_selected(Assignment::Tv);
        assert_eq!(app.assignment_counts(), (0, 2, 1));
        app.mark_selected(Assignment::Unassigned);
        assert_eq!(app.assignment_counts(), (0, 0, 3));
    }

    #[test]
    fn input_error_covers_source_and_destination() {
        let scratch = Scratch::new();
        let mut app = app_with_source(scratch.path());
        assert!(app.input_error().is_none());

        // Destination that does not exist yet but has an existing parent.
        app.dest = scratch.path().join("library").display().to_string();
        assert!(app.input_error().is_none());

        // Destination that is a file.
        let file = scratch.path().join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        app.dest = file.display().to_string();
        assert!(app
            .input_error()
            .unwrap()
            .contains("destination is not a directory"));

        // Destination whose nearest existing ancestor is a file.
        app.dest = file.join("child").join("grandchild").display().to_string();
        assert!(app
            .input_error()
            .unwrap()
            .contains("destination parent is not a directory"));

        // Bad source wins.
        app.source = "definitely/not/a/real/dir".to_string();
        assert!(app.input_error().unwrap().contains("not a directory"));
    }

    #[test]
    fn can_start_needs_an_assignment() {
        let scratch = Scratch::new();
        std::fs::create_dir(scratch.path().join("movie")).unwrap();
        let mut app = app_with_source(scratch.path());
        assert!(!app.can_start());

        app.selected = app.children.iter().cloned().collect();
        app.mark_selected(Assignment::Movies);
        assert!(app.can_start());
    }

    #[test]
    fn format_event_covers_every_variant() {
        let p = |s: &str| PathBuf::from(s);
        let cases = [
            (LogEvent::Scanning, "SCANNING"),
            (LogEvent::JobStarted(p("a")), "started"),
            (LogEvent::JobFinished(p("a")), "finished"),
            (LogEvent::CreateDir(p("a")), "CREATE"),
            (
                LogEvent::PlannedMove {
                    from: p("a"),
                    to: p("b"),
                },
                "MOVE",
            ),
            (
                LogEvent::Moved {
                    from: p("a"),
                    to: p("b"),
                },
                "->",
            ),
            (
                LogEvent::Skipped {
                    path: p("a"),
                    reason: "exists".into(),
                },
                "SKIP",
            ),
            (
                LogEvent::Failed {
                    path: p("a"),
                    reason: "boom".into(),
                },
                "FAIL",
            ),
            (
                LogEvent::Finished {
                    moved: 1,
                    merged: 2,
                    skipped: 3,
                    failed: 4,
                    cancelled: false,
                },
                "Summary: 1 moved, 2 merged, 3 skipped",
            ),
        ];
        for (event, expected) in cases {
            assert!(format_event(&event).contains(expected), "{event:?}");
        }
    }

    #[test]
    fn log_and_status_colors_are_distinct() {
        let visuals = egui::Visuals::dark();
        let p = Palette::DARK;
        assert_eq!(log_line_color("FAIL x", &p, &visuals), visuals.error_fg_color);
        assert_eq!(log_line_color("SKIP x", &p, &visuals), visuals.warn_fg_color);
        assert_eq!(log_line_color("Summary: x", &p, &visuals), p.accent);
        assert_eq!(log_line_color("MOVE x", &p, &visuals), p.text);

        assert_eq!(status_color(Status::Done, &p, &visuals), p.ok);
        assert_eq!(status_color(Status::Failed, &p, &visuals), visuals.error_fg_color);
        assert_eq!(status_color(Status::Cancelled, &p, &visuals), visuals.warn_fg_color);
        assert_eq!(status_color(Status::Idle, &p, &visuals), p.accent);
    }

    fn attach_run(app: &mut GuiApp) -> std::sync::mpsc::Sender<LogEvent> {
        let (tx, rx) = channel();
        app.run = Some(RunHandle {
            receiver: rx,
            cancel: CancelToken::new(),
            join: None,
        });
        tx
    }

    #[test]
    fn drain_events_tracks_status_and_summary() {
        let mut app = GuiApp::default();
        let tx = attach_run(&mut app);

        tx.send(LogEvent::Scanning).unwrap();
        tx.send(LogEvent::PlannedMove {
            from: "a".into(),
            to: "b".into(),
        })
        .unwrap();
        app.drain_events();
        assert_eq!(app.status, Status::Planning);
        assert_eq!(app.log.len(), 2);
        assert!(app.run.is_some());

        app.apply = true;
        tx.send(LogEvent::Moved {
            from: "a".into(),
            to: "b".into(),
        })
        .unwrap();
        app.drain_events();
        assert_eq!(app.status, Status::Applying);

        tx.send(LogEvent::Finished {
            moved: 1,
            merged: 0,
            skipped: 0,
            failed: 0,
            cancelled: false,
        })
        .unwrap();
        app.drain_events();
        assert_eq!(app.status, Status::Done);
        assert_eq!(app.last_summary, Some((1, 0, 0, 0, false)));
        assert!(app.run.is_none());
    }

    #[test]
    fn drain_events_maps_failed_and_cancelled() {
        for (failed, cancelled, expected) in [
            (1, false, Status::Failed),
            (0, true, Status::Cancelled),
        ] {
            let mut app = GuiApp::default();
            let tx = attach_run(&mut app);
            tx.send(LogEvent::Finished {
                moved: 0,
                merged: 0,
                skipped: 0,
                failed,
                cancelled,
            })
            .unwrap();
            app.drain_events();
            assert_eq!(app.status, expected);
        }
    }

    #[test]
    fn drain_events_ends_run_when_sender_disconnects() {
        let mut app = GuiApp::default();
        drop(attach_run(&mut app));
        app.drain_events();
        assert!(app.run.is_none());
    }

    #[test]
    fn stop_cancels_the_active_run() {
        let mut app = GuiApp::default();
        let _tx = attach_run(&mut app);
        app.stop();
        assert!(app.run.as_ref().unwrap().cancel.is_cancelled());

        // No active run: nothing to do.
        let mut idle = GuiApp::default();
        idle.stop();
    }

    #[test]
    fn start_rejects_invalid_input_and_empty_selection() {
        let mut app = GuiApp::default();
        app.start();
        assert!(app.error.is_some());
        assert!(app.run.is_none());

        let scratch = Scratch::new();
        std::fs::create_dir(scratch.path().join("movie")).unwrap();
        let mut app = app_with_source(scratch.path());
        app.start();
        assert!(app.run.is_none(), "nothing assigned, so nothing runs");
    }

    #[test]
    fn preview_run_completes_without_touching_files() {
        let scratch = Scratch::new();
        let movie = scratch.path().join("Inception.2010.1080p");
        std::fs::create_dir(&movie).unwrap();
        std::fs::write(movie.join("Inception.2010.1080p.mkv"), b"data").unwrap();

        let mut app = app_with_source(scratch.path());
        app.selected = app.children.iter().cloned().collect();
        app.mark_selected(Assignment::Movies);
        app.apply = false;
        app.start();
        assert!(app.run.is_some());

        let deadline = Instant::now() + Duration::from_secs(20);
        while app.run.is_some() && Instant::now() < deadline {
            app.drain_events();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(app.run.is_none(), "run did not finish in time");
        assert!(matches!(app.status, Status::Done | Status::Failed));
        assert!(!app.log.is_empty());
        assert!(movie.join("Inception.2010.1080p.mkv").exists());
    }

    /// Renders every card headlessly so the layout and painting code is
    /// exercised in both themes and in the main states.
    #[test]
    fn renders_all_states_in_both_themes() {
        let scratch = Scratch::new();
        for name in ["Inception.2010", "Severance.S01", "Interstellar.2014"] {
            std::fs::create_dir(scratch.path().join(name)).unwrap();
        }

        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            ctx.set_theme(theme);

            let mut app = app_with_source(scratch.path());
            app.dest = scratch.path().join("library").display().to_string();
            app.selected = app.children.iter().take(2).cloned().collect();
            app.assignments
                .insert(app.children[0].clone(), Assignment::Movies);
            app.assignments
                .insert(app.children[1].clone(), Assignment::Tv);
            app.log = vec![
                "MOVE    a -> b".to_string(),
                "SKIP    c (exists)".to_string(),
                "FAIL    d (boom)".to_string(),
                "Summary: 1 moved, 0 merged, 1 skipped, 1 failed, cancelled=false".to_string(),
            ];
            app.last_summary = Some((1, 0, 1, 1, true));
            app.error = Some("something went wrong".to_string());
            app.extra_height = 120.0;

            // Idle, preview mode / apply mode (shows the warning) / running.
            for (apply, running) in [(false, false), (true, false), (true, true)] {
                app.apply = apply;
                app.status = if running { Status::Applying } else { Status::Done };
                let tx = running.then(|| attach_run(&mut app));
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(940.0, 860.0),
                    )),
                    ..Default::default()
                };
                let _ = ctx.run_ui(input, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        widgets::theme_switcher(ui);
                        app.folders_card(ui, running);
                        app.assign_card(ui, running);
                        app.run_card(ui, running);
                        app.log_card(ui, running);
                    });
                });
                drop(tx);
                app.run = None;
            }

            // Empty list placeholder.
            let mut empty = GuiApp::default();
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                empty.assign_card(ui, false);
                empty.log_card(ui, false);
            });
        }
    }
}
