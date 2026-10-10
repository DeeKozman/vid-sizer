// No console window in release; the terminal is only useful while debugging.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod doc;
#[cfg(test)]
mod e2e;
mod ffmpeg;
mod job;
mod logo;
mod player;
mod probe;
mod sizing;
mod ui;

use eframe::egui;

fn main() -> eframe::Result {
    // Anything passed on the command line opens as a tab, so "Open with" and
    // dropping files onto the exe both work.
    let args: Vec<std::path::PathBuf> = std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_file())
        .collect();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1240.0, 960.0])
            .with_min_inner_size([940.0, 700.0])
            .with_icon(logo::window_icon())
            .with_title("Vid Sizer"),
        ..Default::default()
    };

    eframe::run_native(
        "Vid Sizer",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(app::VidSizer::with_args(args)))
        }),
    )
}
