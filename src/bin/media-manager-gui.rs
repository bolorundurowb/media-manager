// Keep a console window from flashing when launched from Explorer; all
// user-visible progress lives in the log panel instead of a terminal.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[path = "../gui.rs"]
mod gui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "media-manager",
        options,
        Box::new(|_cc| Ok(Box::new(gui::GuiApp::default()))),
    )
}
