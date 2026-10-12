// Keep a console window from flashing when launched from Explorer; all
// user-visible progress lives in the log panel instead of a terminal.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[path = "../gui.rs"]
mod gui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Media Manager – Jellyfin Organizer")
            .with_inner_size([940.0, 860.0])
            .with_min_inner_size([760.0, 720.0]),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "Media Manager – Jellyfin Organizer",
        options,
        Box::new(|cc| Ok(Box::new(gui::GuiApp::new(cc)))),
    )
}
