// Keep a console window from flashing when launched from Explorer; all
// user-visible progress lives in the log panel instead of a terminal.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[path = "../gui.rs"]
mod gui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 820.0])
            .with_min_inner_size([680.0, 640.0]),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "Media Manager",
        options,
        Box::new(|_cc| Ok(Box::new(gui::GuiApp::default()))),
    )
}
