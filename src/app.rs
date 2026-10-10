//! The application shell: the header, the tab strip, and routing input to
//! whichever document is in front.

use std::path::PathBuf;

use eframe::egui;
use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::doc::{Doc, Keys};
use crate::ffmpeg::Tools;
use crate::logo::logo_art;
use crate::ui::*;

/// How tall the logo is drawn in the header band, and how wide it is allowed
/// to get. The artwork carries about a quarter of its own height as margin,
/// so the height is set higher than the wordmark actually reads.
const LOGO_HEIGHT: f32 = 74.0;
const LOGO_MAX_WIDTH: f32 = 260.0;

const TAB_BG: Color32 = Color32::from_rgb(30, 33, 40);
const TAB_ACTIVE: Color32 = Color32::from_rgb(48, 54, 66);

pub struct VidSizer {
    tools: Result<Tools, String>,
    docs: Vec<Doc>,
    active: usize,
    load_err: Option<String>,
    logo: Option<egui::TextureHandle>,
    /// Whether a text box held the keyboard last frame, so transport keys do
    /// not fire while typing.
    text_focused: bool,
    /// Files named on the command line, opened on the first frame.
    pending: Vec<PathBuf>,
    show_options: bool,
    options_page: OptionsPage,
}

/// The pages listed down the side of the options dialog. Only one so far,
/// but the list is the point — more drop in beside it.
#[derive(PartialEq, Eq, Clone, Copy)]
enum OptionsPage {
    Shortcuts,
}

impl OptionsPage {
    const ALL: &'static [(OptionsPage, &'static str)] = &[(OptionsPage::Shortcuts, "Shortcuts")];
}

/// Key, what it does. Kept as data so the dialog and the README stay in step.
/// The keys drawn as chips, the word that goes between them, and what they do.
type KeyRow = (&'static [&'static str], &'static str, &'static str);

const PLAYBACK: &[KeyRow] = &[
    (&["2", "Space"], "or", "Play / pause"),
    (&["1", "←"], "or", "Back one frame"),
    (&["3", "→"], "or", "Forward one frame"),
    (&["Home"], "", "Back to the start"),
];

const TIMELINE: &[KeyRow] = &[(&["+", "−"], "/", "Zoom in / out")];

const MARKS: &[KeyRow] = &[
    (&["I"], "", "Set the in marker"),
    (&["O"], "", "Set the out marker"),
    (&["C"], "", "Save frame as PNG"),
];

const TABS: &[KeyRow] = &[
    (&["Ctrl", "Tab"], "+", "Next tab"),
    (&["Ctrl", "Shift", "Tab"], "+", "Previous tab"),
    (&["Ctrl", "W"], "+", "Close the tab"),
    (&["Esc"], "", "Close this dialog"),
];

/// Grouped into the columns they are drawn in, so the layout is data too.
const KEY_COLUMNS: &[&[(&str, &[KeyRow])]] = &[
    &[("Playback", PLAYBACK), ("Timeline", TIMELINE)],
    &[("Markers & capture", MARKS), ("Tabs", TABS)],
];

/// Width set aside for the key chips, so descriptions line up.
const KEY_COL: f32 = 150.0;

/// Things you do with the mouse that are not obvious from looking.
const MOUSE_HINTS: &[(&str, &str)] = &[
    ("Drag the timeline", "Moves the playhead, seeks on release"),
    ("Scroll over the timeline", "Zooms around the pointer"),
    ("Drag inside the crop box", "Moves the crop"),
    ("Drag a crop handle", "Resizes from that edge or corner"),
    ("Drag the preview", "Moves an automatic aspect crop"),
    ("Drop files on the window", "Opens each one in its own tab"),
];

impl Default for VidSizer {
    fn default() -> Self {
        Self {
            tools: Tools::locate(),
            docs: Vec::new(),
            active: 0,
            load_err: None,
            logo: None,
            text_focused: false,
            pending: Vec::new(),
            show_options: false,
            options_page: OptionsPage::Shortcuts,
        }
    }
}

impl VidSizer {
    /// Files named on the command line, so "Open with" and dropping onto the
    /// exe both work. They open on the first frame, once there is a context
    /// to build players against.
    pub fn with_args(paths: Vec<PathBuf>) -> Self {
        Self {
            pending: paths,
            ..Self::default()
        }
    }

    /// Open files as new tabs and bring the last one to the front.
    fn open_paths(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        let Ok(tools) = self.tools.clone() else {
            return;
        };
        let mut opened = false;

        for path in paths.into_iter().filter(|p| p.is_file()) {
            match Doc::open(&tools, &path) {
                Ok(doc) => {
                    self.docs.push(doc);
                    opened = true;
                    self.load_err = None;
                }
                Err(e) => self.load_err = Some(format!("{}: {e}", path.display())),
            }
        }

        if opened {
            self.set_active(self.docs.len() - 1, ctx);
        }
    }

    /// Only the document in front decodes, so switching tabs hands the
    /// player over.
    fn set_active(&mut self, index: usize, ctx: &egui::Context) {
        let Ok(tools) = self.tools.clone() else {
            return;
        };
        if index >= self.docs.len() {
            return;
        }
        if let Some(old) = self.docs.get_mut(self.active) {
            old.deactivate();
        }
        self.active = index;
        if let Some(doc) = self.docs.get_mut(index) {
            doc.activate(&tools, ctx);
        }
    }

    fn close_tab(&mut self, index: usize, ctx: &egui::Context) {
        if index >= self.docs.len() {
            return;
        }
        self.docs.remove(index);
        if self.docs.is_empty() {
            self.active = 0;
            return;
        }
        let next = index.min(self.docs.len() - 1);
        // The removed tab was the live one, so whatever takes its place has
        // to start decoding.
        self.active = usize::MAX;
        self.set_active(next, ctx);
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            self.open_paths(dropped, ctx);
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        // The options dialog swallows them, so reading the shortcut list does
        // not fire the shortcuts.
        if self.text_focused || self.show_options || self.docs.is_empty() {
            return;
        }

        let (keys, next_tab, prev_tab, close) = ctx.input(|i| {
            let ctrl = i.modifiers.ctrl;
            (
                Keys {
                    play: i.key_pressed(egui::Key::Space) || i.key_pressed(egui::Key::Num2),
                    back: i.key_pressed(egui::Key::Num1) || i.key_pressed(egui::Key::ArrowLeft),
                    forward: i.key_pressed(egui::Key::Num3) || i.key_pressed(egui::Key::ArrowRight),
                    zoom_in: i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals),
                    zoom_out: i.key_pressed(egui::Key::Minus),
                    mark_in: i.key_pressed(egui::Key::I),
                    mark_out: i.key_pressed(egui::Key::O),
                    home: i.key_pressed(egui::Key::Home),
                    grab: i.key_pressed(egui::Key::C) && !ctrl,
                },
                ctrl && i.key_pressed(egui::Key::Tab) && !i.modifiers.shift,
                ctrl && i.key_pressed(egui::Key::Tab) && i.modifiers.shift,
                ctrl && i.key_pressed(egui::Key::W),
            )
        });

        if close {
            self.close_tab(self.active, ctx);
            return;
        }
        if next_tab || prev_tab {
            let n = self.docs.len();
            let to = if next_tab {
                (self.active + 1) % n
            } else {
                (self.active + n - 1) % n
            };
            self.set_active(to, ctx);
            return;
        }

        if let Ok(tools) = self.tools.clone() {
            if let Some(doc) = self.docs.get_mut(self.active) {
                doc.handle_keys(keys, &tools, ctx);
            }
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // The logo carries its own background, so the band is painted to
        // match it and the artwork has no visible edge.
        egui::Frame::new()
            .fill(logo_art().background)
            .inner_margin(egui::Margin {
                left: 18,
                right: 20,
                top: 12,
                bottom: 12,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let logo = self.logo.get_or_insert_with(|| {
                        let art = logo_art();
                        ui.ctx().load_texture(
                            "logo",
                            egui::ColorImage::from_rgba_unmultiplied(
                                [art.width as usize, art.height as usize],
                                &art.rgba,
                            ),
                            egui::TextureOptions::LINEAR,
                        )
                    });
                    // Fit inside the band by height, but never let a wide
                    // wordmark crowd out the buttons.
                    let size = logo.size_vec2();
                    let scale = (LOGO_HEIGHT / size.y.max(1.0))
                        .min(LOGO_MAX_WIDTH / size.x.max(1.0))
                        .min((ui.available_width() * 0.45) / size.x.max(1.0));
                    let (mark, _) = ui.allocate_exact_size(size * scale, Sense::hover());
                    ui.painter().image(
                        logo.id(),
                        mark,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );

                    // The artwork is the wordmark, so only the tagline is set
                    // in type beside it.
                    ui.add_space(14.0);
                    ui.label(
                        egui::RichText::new("resize · convert · crop · edit")
                            .size(13.0)
                            .color(Color32::from_rgb(140, 148, 164)),
                    );

                    ui.add_space(10.0);
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("⚙").size(18.0))
                                .min_size(Vec2::new(32.0, 32.0))
                                .frame(false),
                        )
                        .on_hover_text("Options")
                        .clicked()
                    {
                        self.show_options = true;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(egui::RichText::new("Open video…").size(14.0))
                                    .min_size(Vec2::new(132.0, 40.0)),
                            )
                            .clicked()
                        {
                            if let Some(paths) = rfd::FileDialog::new()
                                .add_filter(
                                    "Video",
                                    &[
                                        "mp4", "mov", "mkv", "avi", "webm", "m4v", "mpg", "mpeg",
                                        "wmv", "flv", "ts", "m2ts", "mts", "mxf", "3gp", "ogv",
                                        "vob", "asf", "f4v", "divx",
                                    ],
                                )
                                .add_filter("All files", &["*"])
                                .pick_files()
                            {
                                self.open_paths(paths, ctx);
                            }
                        }
                    });
                });
            });
        ui.separator();
    }

    /// One row of tabs, each with a close cross, plus a + to open more.
    fn tab_strip(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.docs.is_empty() {
            return;
        }

        let mut switch_to = None;
        let mut close = None;

        egui::Frame::new()
            .fill(TAB_BG)
            .inner_margin(egui::Margin {
                left: 8,
                right: 8,
                top: 5,
                bottom: 0,
            })
            .show(ui, |ui| {
                egui::ScrollArea::horizontal()
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            for (i, doc) in self.docs.iter().enumerate() {
                                let is_active = i == self.active;
                                let mut label = doc.title();
                                if label.chars().count() > 26 {
                                    label = label.chars().take(24).collect::<String>() + "…";
                                }
                                if doc.is_running() {
                                    label.push_str("  ⏳");
                                }

                                let fill = if is_active { TAB_ACTIVE } else { TAB_BG };
                                egui::Frame::new()
                                    .fill(fill)
                                    .corner_radius(CornerRadius {
                                        nw: 5,
                                        ne: 5,
                                        sw: 0,
                                        se: 0,
                                    })
                                    .inner_margin(egui::Margin {
                                        left: 9,
                                        right: 5,
                                        top: 4,
                                        bottom: 4,
                                    })
                                    .show(ui, |ui| {
                                        let text = egui::RichText::new(label).size(12.5).color(
                                            if is_active {
                                                Color32::from_rgb(235, 240, 248)
                                            } else {
                                                Color32::from_rgb(150, 158, 172)
                                            },
                                        );
                                        if ui
                                            .add(egui::Label::new(text).sense(Sense::click()))
                                            .on_hover_text(doc.info.path.display().to_string())
                                            .clicked()
                                        {
                                            switch_to = Some(i);
                                        }
                                        if ui
                                            .add(
                                                egui::Label::new(
                                                    egui::RichText::new("×").size(14.0).weak(),
                                                )
                                                .sense(Sense::click()),
                                            )
                                            .on_hover_text("Close this tab (Ctrl+W)")
                                            .clicked()
                                        {
                                            close = Some(i);
                                        }
                                    });
                            }

                            if ui
                                .add(egui::Button::new("+").min_size(Vec2::new(26.0, 24.0)))
                                .on_hover_text("Open another video")
                                .clicked()
                            {
                                if let Some(paths) = rfd::FileDialog::new()
                                    .add_filter("All files", &["*"])
                                    .pick_files()
                                {
                                    self.open_paths(paths, ctx);
                                }
                            }
                        });
                    });
            });

        // Act after the loop, so the list is not mutated while borrowed.
        if let Some(i) = close {
            self.close_tab(i, ctx);
        } else if let Some(i) = switch_to {
            if i != self.active {
                self.set_active(i, ctx);
            }
        }
    }

    /// The options dialog: a list of pages down the left, the chosen one on
    /// the right. Closes on the backdrop, the button or Escape.
    fn options_modal(&mut self, ctx: &egui::Context) {
        if !self.show_options {
            return;
        }

        let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));

        let modal = egui::Modal::new(egui::Id::new("options"))
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(26, 29, 36))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(62, 69, 84)))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(22, 18)),
            )
            .show(ctx, |ui| {
                ui.set_width(1020.0);

                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Options")
                            .size(23.0)
                            .strong()
                            .color(Color32::from_rgb(236, 241, 250)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new("Close").min_size(Vec2::new(78.0, 28.0)))
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(14.0);

                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(132.0, 240.0),
                        egui::Layout::top_down_justified(egui::Align::Min),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            ui.spacing_mut().button_padding = Vec2::new(10.0, 6.0);
                            for (page, name) in OptionsPage::ALL {
                                ui.selectable_value(
                                    &mut self.options_page,
                                    *page,
                                    egui::RichText::new(*name).size(13.5),
                                );
                            }
                        },
                    );
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(10.0);
                    // No scroll area here: inside a modal it caps the height
                    // and silently clips the last rows. The content is small
                    // and fixed, so the dialog sizes itself to it.
                    ui.vertical(|ui| match self.options_page {
                        OptionsPage::Shortcuts => Self::shortcuts_page(ui),
                    });
                });
            });

        if modal.backdrop_response.clicked() || close {
            self.show_options = false;
        }
    }

    fn shortcuts_page(ui: &mut egui::Ui) {
        // Two columns of grouped keys and one of mouse gestures; that split
        // keeps the three roughly the same height.
        ui.columns(3, |cols| {
            for (i, groups) in KEY_COLUMNS.iter().enumerate() {
                for (title, rows) in groups.iter() {
                    Self::group_heading(&mut cols[i], title);
                    for (keys, sep, what) in rows.iter() {
                        Self::key_row(&mut cols[i], keys, sep, what);
                    }
                    cols[i].add_space(14.0);
                }
            }

            Self::group_heading(&mut cols[2], "Mouse");
            for (how, what) in MOUSE_HINTS {
                // Two lines, because a gesture needs more words than a key.
                cols[2].add_space(2.0);
                cols[2].label(egui::RichText::new(*how).size(12.5).color(ACCENT));
                cols[2].label(egui::RichText::new(*what).size(11.5).color(DIM));
            }
        });

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "Keyboard shortcuts are ignored while a text box has the keyboard.",
            )
            .weak()
            .size(11.5),
        );
    }

    fn group_heading(ui: &mut egui::Ui, title: &str) {
        ui.label(
            egui::RichText::new(title.to_uppercase())
                .size(10.5)
                .strong()
                .color(Color32::from_rgb(120, 180, 230)),
        );
        ui.add_space(5.0);
    }

    /// One shortcut: its keys as little keycaps, then what it does.
    fn key_row(ui: &mut egui::Ui, keys: &[&str], sep: &str, what: &str) {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(KEY_COL, 24.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (i, key) in keys.iter().enumerate() {
                        if i > 0 && !sep.is_empty() {
                            ui.label(egui::RichText::new(sep).size(11.0).color(DIM));
                        }
                        Self::keycap(ui, key);
                    }
                },
            );
            ui.add(egui::Label::new(egui::RichText::new(what).size(12.5)).wrap());
        });
        ui.add_space(3.0);
    }

    fn keycap(ui: &mut egui::Ui, label: &str) {
        egui::Frame::new()
            .fill(Color32::from_rgb(44, 49, 61))
            .stroke(Stroke::new(1.0, Color32::from_rgb(76, 84, 101)))
            .corner_radius(CornerRadius::same(4))
            .inner_margin(egui::Margin::symmetric(7, 3))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(label)
                        .monospace()
                        .size(12.0)
                        .color(Color32::from_rgb(224, 232, 244)),
                );
            });
    }

    fn drop_zone(&mut self, ui: &mut egui::Ui) {
        let hovering = ui.input(|i| !i.raw.hovered_files.is_empty());
        let size = Vec2::new(ui.available_width(), ui.available_height().max(120.0));
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());

        let (fill, line) = if hovering {
            (
                Color32::from_rgb(38, 48, 40),
                Color32::from_rgb(120, 210, 140),
            )
        } else {
            (Color32::from_rgb(28, 30, 36), Color32::from_rgb(70, 76, 90))
        };
        let p = ui.painter();
        p.rect(
            rect,
            CornerRadius::same(10),
            fill,
            Stroke::new(1.5, line),
            StrokeKind::Inside,
        );
        p.text(
            rect.center() - Vec2::new(0.0, 14.0),
            egui::Align2::CENTER_CENTER,
            if hovering {
                "Drop them"
            } else {
                "Drag videos here"
            },
            egui::FontId::proportional(22.0),
            Color32::from_rgb(210, 216, 226),
        );
        p.text(
            rect.center() + Vec2::new(0.0, 16.0),
            egui::Align2::CENTER_CENTER,
            "each one opens in its own tab",
            egui::FontId::proportional(13.0),
            Color32::from_rgb(130, 138, 152),
        );

        if let Some(err) = &self.load_err {
            ui.colored_label(BAD, err);
        }
    }
}

impl eframe::App for VidSizer {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if !self.pending.is_empty() {
            let paths = std::mem::take(&mut self.pending);
            self.open_paths(paths, &ctx);
        }

        // Background tabs still need their render jobs serviced.
        for doc in &mut self.docs {
            doc.drain_job();
        }
        self.handle_drops(&ctx);
        // Uses last frame's focus state, which is set as the text boxes are
        // drawn further down.
        self.handle_keys(&ctx);
        self.text_focused = false;
        if let Some(doc) = self.docs.get_mut(self.active) {
            doc.pump_player(&ctx);
        }

        egui::Frame::new()
            .fill(ui.style().visuals.panel_fill)
            .show(ui, |ui| {
                self.header(ui, &ctx);

                if let Err(e) = &self.tools.clone() {
                    egui::Frame::new().inner_margin(14).show(ui, |ui| {
                        ui.colored_label(BAD, format!("⚠ {e}"));
                        ui.label(
                            "Put ffmpeg.exe and ffprobe.exe on PATH or beside this app, then restart.",
                        );
                    });
                    return;
                }

                self.tab_strip(ui, &ctx);

                egui::Frame::new().inner_margin(14).show(ui, |ui| {
                    if self.docs.is_empty() {
                        self.drop_zone(ui);
                        return;
                    }
                    let tools = self.tools.clone().expect("checked above");
                    let mut focused = self.text_focused;
                    if let Some(doc) = self.docs.get_mut(self.active) {
                        doc.ui(ui, &tools, &ctx, &mut focused);
                    }
                    self.text_focused = focused;
                });
            });

        self.options_modal(&ctx);
    }
}
