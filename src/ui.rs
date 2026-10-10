//! Small shared widgets and palette.

use eframe::egui;
use egui::{Color32, Vec2};

pub const RED_FILL: Color32 = Color32::from_rgba_premultiplied(90, 10, 10, 90);
pub const RED_LINE: Color32 = Color32::from_rgb(255, 60, 60);
pub const HEADER_BG: Color32 = Color32::from_rgb(24, 27, 34);
pub const MARK_IN: Color32 = Color32::from_rgb(90, 215, 140);
pub const MARK_OUT: Color32 = Color32::from_rgb(245, 170, 70);
pub const DIM: Color32 = Color32::from_rgb(140, 150, 165);
pub const GOOD: Color32 = Color32::from_rgb(150, 220, 170);
pub const BAD: Color32 = Color32::from_rgb(255, 140, 140);
pub const WARN: Color32 = Color32::from_rgb(240, 200, 110);
pub const ACCENT: Color32 = Color32::from_rgb(140, 220, 255);

/// A label and value on one line, used throughout the info panel.
pub fn row(ui: &mut egui::Ui, label: &str, value: String) {
    ui.horizontal(|ui| {
        ui.add_sized(
            Vec2::new(104.0, 18.0),
            egui::Label::new(egui::RichText::new(label).weak()).halign(egui::Align::LEFT),
        );
        ui.add(egui::Label::new(egui::RichText::new(value).monospace()).wrap());
    });
}

pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(title.to_uppercase())
            .size(10.5)
            .strong()
            .color(DIM),
    );
    ui.add_space(2.0);
}

/// A button that never keeps keyboard focus, so Space stays bound to
/// play/pause instead of re-triggering whatever was clicked last.
pub fn tbtn(ui: &mut egui::Ui, label: &str, size: Vec2, hover: &str) -> bool {
    let r = ui
        .add(egui::Button::new(label).min_size(size))
        .on_hover_text(hover);
    if r.has_focus() {
        r.surrender_focus();
    }
    r.clicked()
}

/// Like [`tbtn`] but coloured, for the one primary action in a row.
pub fn tbtn_filled(
    ui: &mut egui::Ui,
    label: egui::RichText,
    size: Vec2,
    fill: Color32,
    hover: &str,
) -> bool {
    let r = ui
        .add(egui::Button::new(label).min_size(size).fill(fill))
        .on_hover_text(hover);
    if r.has_focus() {
        r.surrender_focus();
    }
    r.clicked()
}
