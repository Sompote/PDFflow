//! PDF Editor: a small PDF editor with a Photoshop-style interface.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod doc;
mod export;
mod icon_data;
mod icons;
mod render;
mod sig;
mod text;
mod theme;
mod widgets;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod ui_tests;

use eframe::egui;

fn main() -> eframe::Result {
    let path = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([760.0, 480.0])
            .with_title("PDF Editor")
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("PDF Editor", options, Box::new(|cc| Ok(Box::new(app::App::new(cc, path)))))
}
