//! PDFflow: a small PDF editor with a Photoshop-style interface.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod doc;
mod dsig;
mod export;
mod icon_data;
mod icons;
mod path;
mod raster;
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

/// The PDFflow mark, shown as the window and Dock icon and in the menu bar.
pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

fn app_icon() -> std::sync::Arc<egui::IconData> {
    let img = image::load_from_memory(ICON_PNG).expect("bundled icon").to_rgba8();
    let (width, height) = img.dimensions();
    std::sync::Arc::new(egui::IconData { rgba: img.into_raw(), width, height })
}

fn main() -> eframe::Result {
    let path = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([760.0, 480.0])
            .with_title("PDFflow")
            .with_app_id("pdfflow")
            .with_drag_and_drop(true)
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native("PDFflow", options, Box::new(|cc| Ok(Box::new(app::App::new(cc, path)))))
}
