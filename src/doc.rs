//! One open video: its settings, its player, its render job, and its UI.
//!
//! Everything that belongs to a single clip lives here, so the app shell can
//! hold a `Vec<Doc>` and show them as tabs. Only the active document keeps a
//! [`Player`] alive — two ffmpeg processes and an audio stream per background
//! tab would be wasteful, so a tab remembers its position instead and resumes
//! there when you come back to it.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use eframe::egui;
use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::ffmpeg::Tools;
use crate::job::{self, Crop, EncodeSpec, Job, JobMsg};
use crate::player::{self, Player};
use crate::probe::{self, MediaInfo};
use crate::sizing::{self, CropRect, Handle, PRESETS};
use crate::ui::*;

const X264_PRESETS: &[&str] = &[
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
    "slow",
    "slower",
    "veryslow",
];

/// Longest edge of the still we fall back to when playback cannot start.
const POSTER_MAX_WIDTH: u32 = 1280;

/// Vertical space reserved under the picture for the transport controls.
const TRANSPORT_HEIGHT: f32 = 92.0;

/// How close the pointer has to be to an edge to grab that handle.
const HANDLE_GRAB: f32 = 10.0;

/// MP3 rip bitrate.
const MP3_KBPS: u32 = 320;

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Mode {
    Percent,
    Preset,
    Custom,
}

pub enum Status {
    Idle,
    Running,
    Done(PathBuf),
    Failed(String),
    Cancelled,
}

/// Which keys the shell saw this frame, so the active document can act on
/// them without each one polling input separately.
#[derive(Default, Clone, Copy)]
pub struct Keys {
    pub play: bool,
    pub back: bool,
    pub forward: bool,
    pub zoom_in: bool,
    pub zoom_out: bool,
    pub mark_in: bool,
    pub mark_out: bool,
    pub home: bool,
    pub grab: bool,
}

impl Keys {
    pub fn any(&self) -> bool {
        self.play
            || self.back
            || self.forward
            || self.zoom_in
            || self.zoom_out
            || self.mark_in
            || self.mark_out
            || self.home
            || self.grab
    }
}

pub struct Doc {
    pub info: MediaInfo,

    player: Option<Player>,
    player_err: Option<String>,
    frame_tex: Option<egui::TextureHandle>,
    /// Where playback was when this tab last went to the background.
    resume_at: f64,

    scrub: f64,
    scrubbing: bool,
    view_start: f64,
    view_len: f64,
    mark_in: Option<f64>,
    mark_out: Option<f64>,
    quick_edit: bool,
    fast_trim: bool,
    notice: Option<(String, bool)>,

    mode: Mode,
    percent: f32,
    preset_idx: usize,
    custom_w: u32,
    custom_h: u32,
    link_custom: bool,

    /// Pan of the automatic aspect crop, as a 0..1 fraction of the slack.
    pan_x: f32,
    pan_y: f32,

    /// The crop tool is open and the box can be dragged.
    crop_mode: bool,
    /// The box being edited while the tool is open.
    crop_edit: Option<CropRect>,
    /// The crop actually applied to the output.
    crop_applied: Option<CropRect>,
    crop_lock: bool,
    crop_ratio: f64,
    crop_w: u32,
    crop_h: u32,
    /// Handle being dragged, the box as it was when the drag began, and where
    /// the pointer went down.
    drag: Option<(Handle, CropRect, Pos2)>,

    crf: u32,
    x264_preset: usize,
    copy_audio: bool,
    show_cmd: bool,

    job: Option<Job>,
    rx: Option<Receiver<JobMsg>>,
    progress: f32,
    speed: Option<String>,
    status: Status,
}

impl Doc {
    pub fn open(tools: &Tools, path: &std::path::Path) -> Result<Self, String> {
        let info = probe::probe(tools, path)?;
        Ok(Self {
            custom_w: info.width,
            custom_h: info.height,
            crop_w: info.width,
            crop_h: info.height,
            crop_ratio: info.aspect(),
            info,
            player: None,
            player_err: None,
            frame_tex: None,
            resume_at: 0.0,
            scrub: 0.0,
            scrubbing: false,
            view_start: 0.0,
            view_len: 0.0,
            mark_in: None,
            mark_out: None,
            quick_edit: false,
            fast_trim: false,
            notice: None,
            mode: Mode::Percent,
            percent: 100.0,
            preset_idx: 2, // 1080p
            link_custom: true,
            pan_x: 0.5,
            pan_y: 0.5,
            crop_mode: false,
            crop_edit: None,
            crop_applied: None,
            crop_lock: false,
            drag: None,
            crf: 18,
            x264_preset: 5, // medium
            copy_audio: true,
            show_cmd: false,
            job: None,
            rx: None,
            progress: 0.0,
            speed: None,
            status: Status::Idle,
        })
    }

    pub fn title(&self) -> String {
        self.info.file_name()
    }

    pub fn is_running(&self) -> bool {
        matches!(self.status, Status::Running)
    }

    /// Start decoding, picking up where this tab left off.
    pub fn activate(&mut self, tools: &Tools, ctx: &egui::Context) {
        if self.player.is_some() {
            return;
        }
        match Player::open(tools, &self.info, ctx) {
            Ok(mut p) => {
                if self.resume_at > 0.05 {
                    p.seek(self.resume_at, ctx);
                }
                self.player = Some(p);
                self.player_err = None;
            }
            Err(e) => {
                // Playback is the nice path; a still keeps the crop usable.
                self.player_err = Some(e);
                if self.frame_tex.is_none() {
                    if let Ok(img) = job::poster_frame(
                        tools,
                        &self.info.path,
                        self.info.duration,
                        POSTER_MAX_WIDTH,
                    ) {
                        let size = [img.width() as usize, img.height() as usize];
                        let colour = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                        self.frame_tex =
                            Some(ctx.load_texture("frame", colour, egui::TextureOptions::LINEAR));
                    }
                }
            }
        }
    }

    /// Stop decoding but remember where we were. The last frame stays on
    /// screen, so the tab still looks right when you come back.
    pub fn deactivate(&mut self) {
        if let Some(p) = self.player.as_ref() {
            self.resume_at = p.position();
        }
        self.player = None;
    }

    /// Per-frame housekeeping that must happen even for a background tab, so
    /// a render started on one tab keeps reporting while you work on another.
    pub fn drain_job(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut finished = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                JobMsg::Progress { fraction, speed } => {
                    self.progress = fraction;
                    self.speed = speed;
                }
                JobMsg::Done(p) => {
                    self.progress = 1.0;
                    self.status = Status::Done(p);
                    finished = true;
                }
                JobMsg::Failed(e) => {
                    self.status = Status::Failed(e);
                    finished = true;
                }
                JobMsg::Cancelled => {
                    self.status = Status::Cancelled;
                    finished = true;
                }
            }
        }
        if finished {
            self.rx = None;
            self.job = None;
        }
    }

    /// Pull whatever frame is due and push it into the texture.
    pub fn pump_player(&mut self, ctx: &egui::Context) {
        let Some(player) = self.player.as_mut() else {
            return;
        };
        let Some(frame) = player.next_frame(ctx) else {
            return;
        };
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [frame.width as usize, frame.height as usize],
            &frame.rgba,
        );
        match &mut self.frame_tex {
            Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
            None => {
                self.frame_tex =
                    Some(ctx.load_texture("frame", image, egui::TextureOptions::LINEAR))
            }
        }
    }

    pub fn handle_keys(&mut self, keys: Keys, tools: &Tools, ctx: &egui::Context) {
        if self.is_running() || !keys.any() {
            return;
        }
        let mut player = self.player.take();
        if let Some(p) = &mut player {
            let duration = p.duration();
            if keys.play {
                p.toggle(ctx);
            }
            if keys.back {
                p.step(-1, ctx);
            }
            if keys.forward {
                p.step(1, ctx);
            }
            if keys.zoom_in {
                self.zoom_timeline(1.0 / 1.5, p.position(), duration);
            }
            if keys.zoom_out {
                self.zoom_timeline(1.5, p.position(), duration);
            }
            if keys.mark_in {
                self.mark_in = Some(p.position());
                self.quick_edit = true;
            }
            if keys.mark_out {
                self.mark_out = Some(p.position());
                self.quick_edit = true;
            }
            if keys.home {
                p.seek(0.0, ctx);
            }
        }
        self.player = player;
        if keys.grab {
            self.take_screenshot(tools);
        }
    }

    // ------------------------------------------------------------- sizing

    /// The output dimensions implied by the current mode.
    fn target(&self) -> (u32, u32) {
        match self.mode {
            Mode::Percent => {
                // A percentage is of the cropped region, not the whole frame.
                let (bw, bh) = self.source_after_crop();
                let f = self.percent as f64 / 100.0;
                (
                    sizing::even_round(bw as f64 * f),
                    sizing::even_round(bh as f64 * f),
                )
            }
            Mode::Preset => {
                let p = &PRESETS[self.preset_idx.min(PRESETS.len() - 1)];
                (p.width, p.height)
            }
            Mode::Custom => (
                sizing::even_round(self.custom_w.max(2) as f64),
                sizing::even_round(self.custom_h.max(2) as f64),
            ),
        }
    }

    /// The pixels the output is built from: the applied crop if there is one,
    /// otherwise the whole frame.
    fn source_after_crop(&self) -> (u32, u32) {
        match self.crop_applied {
            Some(r) => {
                let (_, _, w, h) = r.to_even(self.info.width, self.info.height);
                (w, h)
            }
            None => (self.info.width, self.info.height),
        }
    }

    /// The crop handed to ffmpeg: a crop applied with the tool wins; failing
    /// that, a preset whose aspect does not match still auto-arms one.
    fn crop(&self) -> Option<Crop> {
        if let Some(r) = self.crop_applied {
            let (x, y, w, h) = r.to_even(self.info.width, self.info.height);
            return Some(Crop { w, h, x, y });
        }

        let (tw, th) = self.target();
        if !sizing::needs_crop(self.info.width, self.info.height, tw, th) {
            return None;
        }
        let (cw, ch) = sizing::max_crop(self.info.width, self.info.height, tw, th);
        let slack_x = self.info.width.saturating_sub(cw);
        let slack_y = self.info.height.saturating_sub(ch);
        Some(Crop {
            w: cw,
            h: ch,
            x: sizing::even_down(self.pan_x.clamp(0.0, 1.0) as f64 * slack_x as f64),
            y: sizing::even_down(self.pan_y.clamp(0.0, 1.0) as f64 * slack_y as f64),
        })
    }

    /// True when the output would stretch the cropped picture out of shape.
    fn would_distort(&self) -> bool {
        let Some(c) = self.crop() else { return false };
        let (tw, th) = self.target();
        if tw == 0 || th == 0 || c.h == 0 {
            return false;
        }
        let from = c.w as f64 / c.h as f64;
        let to = tw as f64 / th as f64;
        (from - to).abs() > 0.004
    }

    fn spec(&self) -> EncodeSpec {
        let (w, h) = self.target();
        EncodeSpec {
            input: self.info.path.clone(),
            output: sizing::output_path(&self.info.path, w, h),
            crop: self.crop(),
            width: w,
            height: h,
            crf: self.crf,
            preset: X264_PRESETS[self.x264_preset.min(X264_PRESETS.len() - 1)].to_owned(),
            duration: self.info.duration,
            copy_audio: self.copy_audio && self.info.has_audio,
            trim: None,
            stream_copy: false,
            mp3_kbps: None,
        }
    }

    /// The marked range, in order, or `None` when there is not a usable one.
    fn marked_range(&self) -> Option<(f64, f64)> {
        let duration = self.info.duration.unwrap_or(0.0);
        if duration <= 0.0 {
            return None;
        }
        let a = self.mark_in.unwrap_or(0.0);
        let b = self.mark_out.unwrap_or(duration);
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        if hi - lo < 0.05 {
            None
        } else {
            Some((lo.max(0.0), hi.min(duration)))
        }
    }

    fn quick_spec(&self) -> Option<EncodeSpec> {
        let (start, end) = self.marked_range()?;
        let mut spec = self.spec();

        // A remux keeps the source frame as it is, so the name has to say the
        // source dimensions rather than the chosen target.
        if self.fast_trim {
            spec.stream_copy = true;
            spec.crop = None;
            spec.width = self.info.width;
            spec.height = self.info.height;
            spec.copy_audio = self.info.has_audio;
        }
        spec.trim = Some((start, end));
        spec.output =
            sizing::trim_output_path(&self.info.path, spec.width, spec.height, start, end);
        Some(spec)
    }

    /// Rip the audio: the marked range if there is one, otherwise all of it.
    fn mp3_spec(&self) -> Option<EncodeSpec> {
        if !self.info.has_audio {
            return None;
        }
        let range = self.marked_range();
        let mut spec = self.spec();
        spec.mp3_kbps = Some(MP3_KBPS);
        spec.trim = range;
        spec.output = sizing::mp3_output_path(&self.info.path, range);
        Some(spec)
    }

    // ------------------------------------------------------------- actions

    fn take_screenshot(&mut self, tools: &Tools) {
        let Some(player) = self.player.as_ref() else {
            self.notice = Some(("no player, cannot grab a frame".into(), true));
            return;
        };

        let at = player.position();
        let dest = sizing::shot_path(&self.info.path, at);
        self.notice = match job::screenshot(tools, &self.info.path, at, &dest) {
            Ok(()) => Some((
                format!(
                    "Saved {}  →  {}",
                    dest.file_name().unwrap_or_default().to_string_lossy(),
                    sizing::shot_folder(&self.info.path).display()
                ),
                false,
            )),
            Err(e) => Some((format!("Screenshot failed: {e}"), true)),
        };
    }

    fn start_job(&mut self, spec: EncodeSpec, tools: &Tools, ctx: &egui::Context) {
        // Decoding a preview while encoding just fights for the same cores.
        if let Some(p) = self.player.as_mut() {
            p.pause();
        }
        let (tx, rx) = channel();
        match job::spawn(tools, spec, tx, ctx.clone()) {
            Ok(j) => {
                self.job = Some(j);
                self.rx = Some(rx);
                self.progress = 0.0;
                self.speed = None;
                self.status = Status::Running;
                self.notice = None;
            }
            Err(e) => self.status = Status::Failed(e),
        }
    }

    fn zoom_timeline(&mut self, factor: f64, centre: f64, duration: f64) {
        if duration <= 0.0 {
            return;
        }
        let shortest = (duration * 0.002).max(0.25).min(duration);
        let old = if self.view_len <= 0.0 {
            duration
        } else {
            self.view_len
        };
        let new = (old * factor).clamp(shortest, duration);
        let frac = if old > 0.0 {
            ((centre - self.view_start) / old).clamp(0.0, 1.0)
        } else {
            0.5
        };
        self.view_len = new;
        self.view_start = (centre - frac * new).clamp(0.0, (duration - new).max(0.0));
    }

    fn follow_playhead(&mut self, position: f64, duration: f64) {
        if self.view_len <= 0.0 || self.view_len >= duration {
            return;
        }
        if position < self.view_start || position > self.view_start + self.view_len {
            self.view_start =
                (position - self.view_len * 0.5).clamp(0.0, (duration - self.view_len).max(0.0));
        }
    }

    // ------------------------------------------------------------- crop tool

    fn open_crop_tool(&mut self) {
        let start = self.crop_applied.unwrap_or(CropRect {
            x: 0.0,
            y: 0.0,
            w: self.info.width as f64,
            h: self.info.height as f64,
        });
        self.crop_ratio = start.aspect();
        self.crop_w = start.w.round() as u32;
        self.crop_h = start.h.round() as u32;
        self.crop_edit = Some(start);
        self.crop_mode = true;
    }

    fn apply_crop(&mut self) {
        let Some(r) = self.crop_edit else { return };
        let (_, _, w, h) = r.to_even(self.info.width, self.info.height);
        self.crop_applied = Some(r);
        // Committing gives you the crop's own pixels, so the output size is
        // set to match and nothing is resampled until you change it.
        self.mode = Mode::Custom;
        self.custom_w = w;
        self.custom_h = h;
        self.percent = 100.0;
        self.crop_mode = false;
        self.drag = None;
        self.notice = Some((format!("Cropped to {w} x {h}"), false));
    }

    fn clear_crop(&mut self) {
        self.crop_applied = None;
        self.crop_edit = None;
        self.crop_mode = false;
        self.drag = None;
    }

    /// Put every setting back the way it was when the file was opened:
    /// crop, output size, markers, timeline zoom and encoding options.
    /// Playback is left alone, so you do not lose your place.
    fn reset(&mut self) {
        self.clear_crop();
        self.crop_lock = false;
        self.crop_ratio = self.info.aspect();
        self.crop_w = self.info.width;
        self.crop_h = self.info.height;

        self.mode = Mode::Percent;
        self.percent = 100.0;
        self.preset_idx = 2; // 1080p
        self.custom_w = self.info.width;
        self.custom_h = self.info.height;
        self.link_custom = true;
        self.pan_x = 0.5;
        self.pan_y = 0.5;

        self.mark_in = None;
        self.mark_out = None;
        self.quick_edit = false;
        self.fast_trim = false;
        self.view_start = 0.0;
        self.view_len = 0.0; // recomputed from the duration
        self.scrubbing = false;

        self.crf = 18;
        self.x264_preset = 5; // medium
        self.copy_audio = true;
        self.show_cmd = false;

        // A finished or failed render is history, not a setting.
        if !self.is_running() {
            self.status = Status::Idle;
            self.progress = 0.0;
        }
        self.notice = Some(("Settings reset".into(), false));
    }

    /// Which part of the box the pointer is over, if any.
    fn hit_handle(box_rect: Rect, p: Pos2) -> Option<Handle> {
        let near = |a: f32, b: f32| (a - b).abs() <= HANDLE_GRAB;
        let outer = box_rect.expand(HANDLE_GRAB);
        if !outer.contains(p) {
            return None;
        }
        let (w, e) = (near(p.x, box_rect.left()), near(p.x, box_rect.right()));
        let (n, s) = (near(p.y, box_rect.top()), near(p.y, box_rect.bottom()));
        Some(match (n, s, w, e) {
            (true, _, true, _) => Handle::NW,
            (true, _, _, true) => Handle::NE,
            (_, true, true, _) => Handle::SW,
            (_, true, _, true) => Handle::SE,
            (true, ..) => Handle::N,
            (_, true, ..) => Handle::S,
            (_, _, true, _) => Handle::W,
            (_, _, _, true) => Handle::E,
            _ if box_rect.contains(p) => Handle::Move,
            _ => return None,
        })
    }

    fn cursor_for(handle: Handle) -> egui::CursorIcon {
        match handle {
            Handle::Move => egui::CursorIcon::Move,
            Handle::N | Handle::S => egui::CursorIcon::ResizeVertical,
            Handle::E | Handle::W => egui::CursorIcon::ResizeHorizontal,
            Handle::NE | Handle::SW => egui::CursorIcon::ResizeNeSw,
            Handle::NW | Handle::SE => egui::CursorIcon::ResizeNwSe,
        }
    }
}

// ------------------------------------------------------------------- the UI

impl Doc {
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        tools: &Tools,
        ctx: &egui::Context,
        text_focused: &mut bool,
    ) {
        ui.horizontal_top(|ui| {
            let left = 400.0_f32.min(ui.available_width() * 0.45);
            ui.allocate_ui_with_layout(
                Vec2::new(left, ui.available_height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(left);
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.left_column(ui, tools, ctx, text_focused));
                },
            );
            ui.separator();
            ui.vertical(|ui| self.preview(ui, tools, ctx));
        });
    }

    fn left_column(
        &mut self,
        ui: &mut egui::Ui,
        tools: &Tools,
        ctx: &egui::Context,
        text_focused: &mut bool,
    ) {
        let info = self.info.clone();
        let (tw, th) = self.target();

        section(ui, "Source");
        ui.label(
            egui::RichText::new(info.file_name())
                .strong()
                .color(Color32::from_rgb(225, 230, 240)),
        );
        ui.label(
            egui::RichText::new(
                info.path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            )
            .weak()
            .size(11.0),
        );
        ui.add_space(4.0);

        row(
            ui,
            "Dimensions",
            format!("{} x {}", info.width, info.height),
        );
        row(
            ui,
            "Aspect",
            format!(
                "{}  ({:.4})",
                sizing::ratio_label(info.width, info.height),
                info.aspect()
            ),
        );
        if info.rotation != 0 {
            row(
                ui,
                "Rotation",
                format!(
                    "{}° (stored {} x {})",
                    info.rotation, info.stored_width, info.stored_height
                ),
            );
        }
        row(
            ui,
            "Duration",
            info.duration
                .map(sizing::human_duration)
                .unwrap_or("-".into()),
        );
        row(
            ui,
            "Frame rate",
            info.fps
                .map(|f| format!("{f:.3} fps"))
                .unwrap_or("-".into()),
        );
        if let Some(n) = info.frames {
            row(ui, "Frames", n.to_string());
        }
        row(ui, "Container", info.container.clone());
        row(
            ui,
            "File size",
            info.size_bytes
                .map(sizing::human_bytes)
                .unwrap_or("-".into()),
        );
        row(
            ui,
            "Bit rate",
            info.bit_rate
                .map(sizing::human_bitrate)
                .unwrap_or("-".into()),
        );

        section(ui, "Video stream");
        row(ui, "Codec", info.v_codec.clone());
        if let Some(p) = &info.v_profile {
            row(ui, "Profile", p.clone());
        }
        row(
            ui,
            "Pixel format",
            info.pix_fmt.clone().unwrap_or("-".into()),
        );
        row(
            ui,
            "SAR / DAR",
            format!(
                "{} / {}",
                info.sar.clone().unwrap_or("-".into()),
                info.dar.clone().unwrap_or("-".into())
            ),
        );
        if let Some(b) = info.v_bit_rate {
            row(ui, "Bit rate", sizing::human_bitrate(b));
        }

        section(ui, "Audio stream");
        if info.has_audio {
            row(ui, "Codec", info.a_codec.clone().unwrap_or("-".into()));
            row(
                ui,
                "Channels",
                match (&info.a_layout, info.a_channels) {
                    (Some(l), _) => l.clone(),
                    (None, Some(c)) => format!("{c} ch"),
                    _ => "-".into(),
                },
            );
            row(
                ui,
                "Sample rate",
                info.a_sample_rate
                    .map(|r| format!("{r} Hz"))
                    .unwrap_or("-".into()),
            );
            if let Some(b) = info.a_bit_rate {
                row(ui, "Bit rate", sizing::human_bitrate(b));
            }
        } else {
            ui.label(egui::RichText::new("none").weak().monospace());
        }

        ui.add_space(10.0);
        ui.separator();
        self.crop_panel(ui);

        ui.add_space(6.0);
        ui.separator();
        section(ui, "New size");

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Percent, "Percent");
            ui.selectable_value(&mut self.mode, Mode::Preset, "Standard size");
            ui.selectable_value(&mut self.mode, Mode::Custom, "Custom");
        });
        ui.add_space(6.0);

        match self.mode {
            Mode::Percent => {
                ui.add(
                    egui::Slider::new(&mut self.percent, 5.0..=200.0)
                        .suffix(" %")
                        .fixed_decimals(1)
                        .text("Scale"),
                );
                ui.horizontal(|ui| {
                    for p in [25.0, 50.0, 75.0, 100.0, 150.0, 200.0] {
                        if ui.small_button(format!("{p:.0}%")).clicked() {
                            self.percent = p;
                        }
                    }
                });
                let note = if self.crop_applied.is_some() {
                    "Percentage of the cropped area; the aspect never changes."
                } else {
                    "Width and height move together, so the aspect never changes."
                };
                ui.label(egui::RichText::new(note).weak().size(11.0));
            }
            Mode::Preset => {
                egui::ComboBox::from_id_salt("preset")
                    .width(260.0)
                    .selected_text(PRESETS[self.preset_idx.min(PRESETS.len() - 1)].label)
                    .show_ui(ui, |ui| {
                        for (i, p) in PRESETS.iter().enumerate() {
                            ui.selectable_value(&mut self.preset_idx, i, p.label);
                        }
                    });
            }
            Mode::Custom => {
                let (bw, bh) = self.source_after_crop();
                let aspect = if bh == 0 { 1.0 } else { bw as f64 / bh as f64 };
                ui.horizontal(|ui| {
                    ui.label("W");
                    let w = ui.add(
                        egui::DragValue::new(&mut self.custom_w)
                            .range(2..=16384)
                            .speed(2.0),
                    );
                    ui.label("H");
                    let h = ui.add(
                        egui::DragValue::new(&mut self.custom_h)
                            .range(2..=16384)
                            .speed(2.0),
                    );
                    if self.link_custom {
                        if w.changed() {
                            self.custom_h = sizing::even_round(self.custom_w as f64 / aspect);
                        } else if h.changed() {
                            self.custom_w = sizing::even_round(self.custom_h as f64 * aspect);
                        }
                    }
                });
                ui.checkbox(&mut self.link_custom, "Keep source aspect ratio");
            }
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Output").weak());
            ui.label(
                egui::RichText::new(format!("{tw} x {th}"))
                    .monospace()
                    .strong()
                    .size(15.0)
                    .color(ACCENT),
            );
            ui.label(egui::RichText::new(sizing::ratio_label(tw, th)).weak());

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if tbtn(
                    ui,
                    "↺ Reset",
                    Vec2::new(72.0, 24.0),
                    "Put the crop, size, markers and encoding options back to how this \
                     file opened. Playback keeps its place.",
                ) {
                    self.reset();
                }
            });
        });

        let (bw, bh) = self.source_after_crop();
        if tw > bw || th > bh {
            ui.colored_label(WARN, "⚠ Upscaling past the source — it will look soft.");
        }
        if self.would_distort() {
            ui.colored_label(
                WARN,
                "⚠ Output aspect differs from the crop — the picture will be stretched.",
            );
        }

        // The automatic crop still handles a preset whose aspect does not
        // match, as long as the crop tool has not been used.
        if self.crop_applied.is_none() {
            match self.crop() {
                Some(c) => {
                    ui.colored_label(
                        RED_LINE,
                        format!("Aspect mismatch — auto-cropping to {} x {}", c.w, c.h),
                    );
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Pan").weak());
                        ui.add(
                            egui::Slider::new(&mut self.pan_x, 0.0..=1.0)
                                .show_value(false)
                                .text("X"),
                        );
                        ui.add(
                            egui::Slider::new(&mut self.pan_y, 0.0..=1.0)
                                .show_value(false)
                                .text("Y"),
                        );
                        if ui.small_button("Center").clicked() {
                            self.pan_x = 0.5;
                            self.pan_y = 0.5;
                        }
                    });
                }
                None => {
                    ui.label(
                        egui::RichText::new("Aspect matches the source — no crop needed.")
                            .weak()
                            .size(11.0),
                    );
                }
            }
        }

        ui.add_space(6.0);
        ui.collapsing("Encoding", |ui| {
            ui.horizontal(|ui| {
                ui.label("Quality (CRF)");
                ui.add(egui::DragValue::new(&mut self.crf).range(0..=51));
                ui.label(
                    egui::RichText::new("lower = better, 18 is near-lossless")
                        .weak()
                        .size(11.0),
                );
            });
            ui.horizontal(|ui| {
                ui.label("x264 preset");
                egui::ComboBox::from_id_salt("x264")
                    .selected_text(X264_PRESETS[self.x264_preset])
                    .show_ui(ui, |ui| {
                        for (i, p) in X264_PRESETS.iter().enumerate() {
                            ui.selectable_value(&mut self.x264_preset, i, *p);
                        }
                    });
            });
            ui.add_enabled(
                info.has_audio,
                egui::Checkbox::new(&mut self.copy_audio, "Copy audio untouched"),
            );
            ui.checkbox(&mut self.show_cmd, "Show the ffmpeg command");
        });

        let spec = self.spec();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Saving as").weak().size(11.0));
        ui.label(
            egui::RichText::new(
                spec.output
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            )
            .monospace()
            .color(GOOD),
        );

        if self.show_cmd {
            let cmd = spec.preview_command(tools);
            ui.add_space(4.0);
            let mut text = cmd.clone();
            let resp = ui.add(
                egui::TextEdit::multiline(&mut text)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY),
            );
            *text_focused |= resp.has_focus();
            if ui.small_button("Copy command").clicked() {
                ui.ctx().copy_text(cmd);
            }
        }

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(4.0);

        if self.is_running() {
            ui.add(
                egui::ProgressBar::new(self.progress)
                    .show_percentage()
                    .desired_height(22.0),
            );
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    if let Some(j) = &self.job {
                        j.cancel();
                    }
                }
                if let Some(s) = &self.speed {
                    ui.label(egui::RichText::new(format!("{s} speed")).weak());
                }
                ui.spinner();
            });
        } else {
            let btn = egui::Button::new(
                egui::RichText::new(format!("Render  {tw} x {th}"))
                    .size(15.0)
                    .strong(),
            )
            .min_size(Vec2::new(ui.available_width(), 34.0))
            .fill(Color32::from_rgb(40, 90, 150));
            if ui.add(btn).clicked() {
                let spec = self.spec();
                self.start_job(spec, tools, ctx);
            }
        }

        match &self.status {
            Status::Done(p) => {
                ui.add_space(4.0);
                ui.colored_label(
                    GOOD,
                    format!(
                        "✔ Saved {}",
                        p.file_name().unwrap_or_default().to_string_lossy()
                    ),
                );
                let folder = p.parent().map(|d| d.to_path_buf());
                if ui.small_button("Show in folder").clicked() {
                    if let Some(d) = folder {
                        let _ = std::process::Command::new("explorer").arg(d).spawn();
                    }
                }
            }
            Status::Failed(e) => {
                ui.add_space(4.0);
                ui.colored_label(BAD, "✘ ffmpeg failed");
                let mut text = e.clone();
                let resp = ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(4)
                        .desired_width(f32::INFINITY),
                );
                *text_focused |= resp.has_focus();
            }
            Status::Cancelled => {
                ui.add_space(4.0);
                ui.colored_label(WARN, "Cancelled — partial file removed.");
            }
            _ => {}
        }
    }

    /// The crop tool's controls: open it, set an exact size, lock the ratio,
    /// apply or cancel.
    fn crop_panel(&mut self, ui: &mut egui::Ui) {
        section(ui, "Crop");

        if !self.crop_mode {
            ui.horizontal(|ui| {
                if tbtn(
                    ui,
                    "✂ Crop",
                    Vec2::new(86.0, 28.0),
                    "Open the crop tool and drag the box on the preview",
                ) {
                    self.open_crop_tool();
                }
                match self.crop_applied {
                    Some(r) => {
                        let (x, y, w, h) = r.to_even(self.info.width, self.info.height);
                        ui.label(
                            egui::RichText::new(format!("{w} x {h} at {x}, {y}"))
                                .monospace()
                                .size(12.0)
                                .color(RED_LINE),
                        );
                        if ui.small_button("Clear").clicked() {
                            self.clear_crop();
                        }
                    }
                    None => {
                        ui.label(egui::RichText::new("whole frame").weak().size(11.0));
                    }
                }
            });
            return;
        }

        // The tool is open: exact size, aspect lock, apply or cancel.
        let (bw, bh) = (self.info.width as f64, self.info.height as f64);
        let mut rect = self.crop_edit.unwrap_or(CropRect {
            x: 0.0,
            y: 0.0,
            w: bw,
            h: bh,
        });
        self.crop_w = rect.w.round() as u32;
        self.crop_h = rect.h.round() as u32;

        ui.horizontal(|ui| {
            ui.label("W");
            let w = ui.add(
                egui::DragValue::new(&mut self.crop_w)
                    .range(16..=self.info.width.max(16))
                    .speed(2.0),
            );
            ui.label("H");
            let h = ui.add(
                egui::DragValue::new(&mut self.crop_h)
                    .range(16..=self.info.height.max(16))
                    .speed(2.0),
            );
            if w.changed() || h.changed() {
                if self.crop_lock && self.crop_ratio > 0.0 {
                    if w.changed() {
                        self.crop_h = (self.crop_w as f64 / self.crop_ratio).round() as u32;
                    } else {
                        self.crop_w = (self.crop_h as f64 * self.crop_ratio).round() as u32;
                    }
                }
                rect = rect.resized(self.crop_w as f64, self.crop_h as f64, bw, bh);
            }
        });

        ui.horizontal(|ui| {
            let was = self.crop_lock;
            ui.checkbox(&mut self.crop_lock, "Lock aspect ratio");
            if self.crop_lock && !was {
                // Lock to whatever is on screen at the moment it is ticked.
                self.crop_ratio = rect.aspect();
            }
            if self.crop_lock {
                ui.label(
                    egui::RichText::new(format!("{:.4}", self.crop_ratio))
                        .monospace()
                        .size(11.0)
                        .weak(),
                );
            }
        });

        ui.horizontal(|ui| {
            if ui.small_button("Full frame").clicked() {
                rect = CropRect {
                    x: 0.0,
                    y: 0.0,
                    w: bw,
                    h: bh,
                };
            }
            if ui.small_button("Centre").clicked() {
                rect = CropRect::centred(bw, bh, rect.w, rect.h);
            }
        });

        self.crop_edit = Some(rect);

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let (_, _, w, h) = rect.to_even(self.info.width, self.info.height);
            if tbtn_filled(
                ui,
                egui::RichText::new(format!("Crop to {w} x {h}")).strong(),
                Vec2::new(150.0, 30.0),
                Color32::from_rgb(150, 50, 50),
                "Apply this crop and set the output size to match",
            ) {
                self.apply_crop();
            }
            if tbtn(
                ui,
                "Cancel",
                Vec2::new(70.0, 30.0),
                "Leave the crop as it was",
            ) {
                self.crop_mode = false;
                self.crop_edit = None;
                self.drag = None;
            }
        });
        ui.label(
            egui::RichText::new("Drag inside the box to move it, the edges and corners to resize.")
                .weak()
                .size(11.0),
        );
    }
}

// ----------------------------------------------------- preview and transport

impl Doc {
    fn preview(&mut self, ui: &mut egui::Ui, tools: &Tools, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("PREVIEW")
                    .size(10.5)
                    .strong()
                    .color(DIM),
            );
            if self.crop_mode {
                ui.label(
                    egui::RichText::new("drag the box or its handles")
                        .size(11.0)
                        .color(RED_LINE),
                );
            } else if self.crop_applied.is_none() && self.crop().is_some() {
                ui.label(
                    egui::RichText::new("drag the frame to reposition the auto crop")
                        .weak()
                        .size(11.0),
                );
            }
        });
        ui.add_space(4.0);

        let Some(tex) = self.frame_tex.clone() else {
            let msg = self
                .player_err
                .clone()
                .unwrap_or_else(|| "no preview frame".to_owned());
            ui.colored_label(WARN, msg);
            return;
        };

        // Fit the picture into what is left after the transport bar, never
        // scaling it up past its own resolution.
        let tex_size = tex.size_vec2();
        let mut reserve = 0.0;
        if self.player.is_some() {
            reserve += TRANSPORT_HEIGHT;
            let duration = self.player.as_ref().map(|p| p.duration()).unwrap_or(0.0);
            if self.view_len > 0.0 && self.view_len < duration - 1e-6 {
                reserve += 24.0;
            }
            if self.quick_edit {
                reserve += 34.0;
            }
            if self.notice.is_some() {
                reserve += 20.0;
            }
        }
        let avail = Vec2::new(
            ui.available_width().max(80.0),
            (ui.available_height() - reserve - 4.0).max(80.0),
        );
        let scale = (avail.x / tex_size.x).min(avail.y / tex_size.y);
        let shown = tex_size * scale;

        let interactive = self.crop_mode || (self.crop_applied.is_none() && self.crop().is_some());
        let sense = if interactive {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        };
        let (rect, resp) = ui.allocate_exact_size(shown, sense);

        let painter = ui.painter_at(rect);
        painter.image(
            tex.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        if self.crop_mode {
            self.crop_overlay(ui, &painter, rect, &resp);
        } else if let Some(applied) = self.crop_applied {
            // Not editing: show where the crop sits, without handles.
            let box_rect = self.source_to_screen(rect, applied);
            Self::dim_outside(&painter, rect, box_rect);
            painter.rect_stroke(
                box_rect,
                CornerRadius::ZERO,
                Stroke::new(2.0, RED_LINE),
                StrokeKind::Inside,
            );
        } else if let Some(c) = self.crop() {
            self.auto_crop_overlay(ui, &painter, rect, &resp, c);
        } else {
            painter.rect_stroke(
                rect,
                CornerRadius::ZERO,
                Stroke::new(1.0, Color32::from_rgb(70, 76, 90)),
                StrokeKind::Inside,
            );
        }

        if self.player.is_some() {
            ui.add_space(6.0);
            self.transport(ui, rect.width(), tools, ctx);
        }
    }

    /// Map a rect in source pixels onto the picture on screen.
    fn source_to_screen(&self, picture: Rect, r: CropRect) -> Rect {
        let sx = picture.width() / self.info.width.max(1) as f32;
        let sy = picture.height() / self.info.height.max(1) as f32;
        Rect::from_min_size(
            picture.min + Vec2::new(r.x as f32 * sx, r.y as f32 * sy),
            Vec2::new(r.w as f32 * sx, r.h as f32 * sy),
        )
    }

    fn dim_outside(painter: &egui::Painter, picture: Rect, keep: Rect) {
        let shade = Color32::from_rgba_premultiplied(0, 0, 0, 130);
        for dropped in [
            Rect::from_min_max(picture.min, Pos2::new(picture.max.x, keep.min.y)),
            Rect::from_min_max(Pos2::new(picture.min.x, keep.max.y), picture.max),
            Rect::from_min_max(
                Pos2::new(picture.min.x, keep.min.y),
                Pos2::new(keep.min.x, keep.max.y),
            ),
            Rect::from_min_max(
                Pos2::new(keep.max.x, keep.min.y),
                Pos2::new(picture.max.x, keep.max.y),
            ),
        ] {
            if dropped.width() > 0.0 && dropped.height() > 0.0 {
                painter.rect_filled(dropped, CornerRadius::ZERO, shade);
            }
        }
    }

    fn thirds(painter: &egui::Painter, b: Rect, colour: Color32) {
        for i in 1..3 {
            let f = i as f32 / 3.0;
            let x = b.min.x + b.width() * f;
            let y = b.min.y + b.height() * f;
            let hair = Stroke::new(1.0, colour);
            painter.line_segment([Pos2::new(x, b.min.y), Pos2::new(x, b.max.y)], hair);
            painter.line_segment([Pos2::new(b.min.x, y), Pos2::new(b.max.x, y)], hair);
        }
    }

    /// The editable crop box: translucent red, dimmed surroundings, eight
    /// handles, dragged in source pixels so it is resolution independent.
    fn crop_overlay(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        picture: Rect,
        resp: &egui::Response,
    ) {
        let (bw, bh) = (self.info.width as f64, self.info.height as f64);
        let mut rect = self.crop_edit.unwrap_or(CropRect {
            x: 0.0,
            y: 0.0,
            w: bw,
            h: bh,
        });
        let box_rect = self.source_to_screen(picture, rect);

        // Screen pixels to source pixels.
        let per_px = bw / picture.width().max(1.0) as f64;

        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let handle = Self::hit_handle(box_rect, p).unwrap_or(Handle::Move);
                self.drag = Some((handle, rect, p));
            }
        }
        if resp.dragged() {
            if let (Some((handle, start, origin)), Some(now)) =
                (self.drag, resp.interact_pointer_pos())
            {
                let dx = (now.x - origin.x) as f64 * per_px;
                let dy = (now.y - origin.y) as f64 * per_px;
                let lock = if self.crop_lock {
                    Some(self.crop_ratio)
                } else {
                    None
                };
                rect = sizing::drag_crop(start, handle, dx, dy, bw, bh, lock);
                self.crop_edit = Some(rect);
            }
        }
        if resp.drag_stopped() {
            self.drag = None;
            if !self.crop_lock {
                // Keep the free ratio in step, so ticking the lock later
                // holds what is actually on screen.
                self.crop_ratio = rect.aspect();
            }
        }

        // Cursor feedback for whichever handle is under the pointer.
        let hovered = resp
            .hover_pos()
            .and_then(|p| Self::hit_handle(box_rect, p))
            .or(self.drag.map(|d| d.0));
        if let Some(h) = hovered {
            ui.ctx().set_cursor_icon(Self::cursor_for(h));
        }

        let box_rect = self.source_to_screen(picture, rect);
        Self::dim_outside(painter, picture, box_rect);
        painter.rect(
            box_rect,
            CornerRadius::ZERO,
            RED_FILL,
            Stroke::new(2.0, RED_LINE),
            StrokeKind::Inside,
        );
        Self::thirds(
            painter,
            box_rect,
            Color32::from_rgba_premultiplied(255, 90, 90, 90),
        );

        // Handles: corners square, edges a short bar.
        let s = 7.0;
        let (l, r, t, b) = (
            box_rect.left(),
            box_rect.right(),
            box_rect.top(),
            box_rect.bottom(),
        );
        let (cx, cy) = (box_rect.center().x, box_rect.center().y);
        for (x, y) in [
            (l, t),
            (r, t),
            (l, b),
            (r, b),
            (cx, t),
            (cx, b),
            (l, cy),
            (r, cy),
        ] {
            painter.rect(
                Rect::from_center_size(Pos2::new(x, y), Vec2::splat(s * 2.0)),
                CornerRadius::same(1),
                Color32::from_rgb(250, 250, 250),
                Stroke::new(1.5, RED_LINE),
                StrokeKind::Inside,
            );
        }

        let (_, _, ew, eh) = rect.to_even(self.info.width, self.info.height);
        painter.text(
            box_rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{ew} x {eh}"),
            egui::FontId::monospace(14.0),
            Color32::from_rgb(255, 230, 230),
        );
    }

    /// The old behaviour, kept for a preset whose aspect does not match: a
    /// max-size box you slide around rather than resize.
    fn auto_crop_overlay(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        picture: Rect,
        resp: &egui::Response,
        c: Crop,
    ) {
        if resp.dragged() {
            let d = resp.drag_delta();
            let per_px = self.info.width as f32 / picture.width().max(1.0);
            let slack_x = self.info.width.saturating_sub(c.w) as f32;
            let slack_y = self.info.height.saturating_sub(c.h) as f32;
            if slack_x > 0.0 {
                self.pan_x = (self.pan_x + d.x * per_px / slack_x).clamp(0.0, 1.0);
            }
            if slack_y > 0.0 {
                self.pan_y = (self.pan_y + d.y * per_px / slack_y).clamp(0.0, 1.0);
            }
        }
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }

        let box_rect = self.source_to_screen(
            picture,
            CropRect {
                x: c.x as f64,
                y: c.y as f64,
                w: c.w as f64,
                h: c.h as f64,
            },
        );
        Self::dim_outside(painter, picture, box_rect);
        painter.rect(
            box_rect,
            CornerRadius::ZERO,
            RED_FILL,
            Stroke::new(2.0, RED_LINE),
            StrokeKind::Inside,
        );
        Self::thirds(
            painter,
            box_rect,
            Color32::from_rgba_premultiplied(255, 90, 90, 90),
        );
        painter.text(
            box_rect.min + Vec2::new(6.0, 4.0),
            egui::Align2::LEFT_TOP,
            format!("{} x {}", c.w, c.h),
            egui::FontId::monospace(12.0),
            Color32::from_rgb(255, 220, 220),
        );
    }

    /// Play/pause, frame stepping, screenshots, the timeline and quick edit.
    fn transport(&mut self, ui: &mut egui::Ui, width: f32, tools: &Tools, ctx: &egui::Context) {
        // Taken out so the borrow checker lets us touch the rest of `self`.
        let mut player = self.player.take();
        let Some(p) = player.as_mut() else {
            self.player = player;
            return;
        };

        let duration = p.duration();
        let position = p.position();
        if !self.scrubbing {
            self.scrub = position;
        }
        if p.is_playing() {
            self.follow_playhead(position, duration);
        }
        let note = p.audio_note.clone();
        let has_audio = self.info.has_audio;
        let mut grab = false;
        let mut export = false;
        let mut rip = false;

        ui.allocate_ui(Vec2::new(width.max(360.0), TRANSPORT_HEIGHT), |ui| {
            ui.horizontal(|ui| {
                let b = Vec2::new(36.0, 30.0);
                if tbtn(ui, "⏮", b, "Back to start (Home)") {
                    p.seek(0.0, ctx);
                }
                if tbtn(ui, "◀", b, "Previous frame (1 or ←)") {
                    p.step(-1, ctx);
                }
                let label = if p.is_playing() { "⏸" } else { "▶" };
                if tbtn_filled(
                    ui,
                    egui::RichText::new(label).size(17.0),
                    Vec2::new(50.0, 30.0),
                    Color32::from_rgb(40, 90, 150),
                    "Play / pause (2 or Space)",
                ) {
                    p.toggle(ctx);
                }
                if tbtn(ui, "▶", b, "Next frame (3 or →)") {
                    p.step(1, ctx);
                }

                ui.label(
                    egui::RichText::new(format!(
                        "{} / {}",
                        player::timecode(self.scrub),
                        player::timecode(duration)
                    ))
                    .monospace()
                    .size(13.0),
                );

                ui.add_space(8.0);
                if tbtn(
                    ui,
                    "📷 Grab",
                    Vec2::new(78.0, 30.0),
                    "Save this frame as a PNG (C)",
                ) {
                    grab = true;
                }
                let edit = ui
                    .selectable_label(self.quick_edit, "✂ Quick edit")
                    .on_hover_text("In / out markers, a trimmed export and MP3");
                if edit.clicked() {
                    self.quick_edit = !self.quick_edit;
                }

                if p.has_audio() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut vol = p.volume();
                        if ui
                            .add_sized(
                                Vec2::new(90.0, 18.0),
                                egui::Slider::new(&mut vol, 0.0..=1.0)
                                    .show_value(false)
                                    .trailing_fill(true),
                            )
                            .changed()
                        {
                            p.set_volume(vol);
                        }
                        ui.label("🔊");
                    });
                }
            });

            ui.add_space(3.0);
            self.timeline(ui, p, ctx);
        });

        if self.quick_edit {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if tbtn(
                    ui,
                    "Set In (I)",
                    Vec2::new(84.0, 26.0),
                    "Mark the start here",
                ) {
                    self.mark_in = Some(position);
                }
                if tbtn(
                    ui,
                    "Set Out (O)",
                    Vec2::new(90.0, 26.0),
                    "Mark the end here",
                ) {
                    self.mark_out = Some(position);
                }
                if tbtn(ui, "Clear", Vec2::new(58.0, 26.0), "Drop both markers") {
                    self.mark_in = None;
                    self.mark_out = None;
                }

                match (self.mark_in, self.mark_out) {
                    (None, None) => {
                        ui.label(egui::RichText::new("no range marked").weak().size(11.0));
                    }
                    _ => {
                        let a = self.mark_in.unwrap_or(0.0);
                        let b = self.mark_out.unwrap_or(duration);
                        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                        ui.label(
                            egui::RichText::new(format!(
                                "{} → {}   ({})",
                                player::timecode(lo),
                                player::timecode(hi),
                                player::timecode(hi - lo)
                            ))
                            .monospace()
                            .size(12.0)
                            .color(MARK_IN),
                        );
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mp3_hint = if self.mark_in.is_some() || self.mark_out.is_some() {
                        "Rip the marked range to MP3 at 320 kbps"
                    } else {
                        "Rip the whole audio track to MP3 at 320 kbps"
                    };
                    if ui
                        .add_enabled(
                            has_audio,
                            egui::Button::new(egui::RichText::new("♪ Save as .mp3").strong())
                                .min_size(Vec2::new(118.0, 26.0))
                                .fill(Color32::from_rgb(95, 70, 140)),
                        )
                        .on_hover_text(mp3_hint)
                        .on_disabled_hover_text("This file has no audio track")
                        .clicked()
                    {
                        rip = true;
                    }
                    if tbtn_filled(
                        ui,
                        egui::RichText::new("Quick export").strong(),
                        Vec2::new(110.0, 26.0),
                        Color32::from_rgb(40, 110, 80),
                        "Render just the marked range",
                    ) {
                        export = true;
                    }
                    ui.checkbox(&mut self.fast_trim, "Fast (copy)")
                        .on_hover_text(
                            "Remux without re-encoding: near instant, but cuts land on \
                             the nearest keyframe and the resize and crop are skipped.",
                        );
                });
            });
        }

        self.player = player;

        if grab {
            self.take_screenshot(tools);
        }
        if export {
            match self.quick_spec() {
                Some(spec) => self.start_job(spec, tools, ctx),
                None => {
                    self.notice = Some((
                        "Set an In and an Out at least a few frames apart first.".into(),
                        true,
                    ))
                }
            }
        }
        if rip {
            match self.mp3_spec() {
                Some(spec) => self.start_job(spec, tools, ctx),
                None => self.notice = Some(("This file has no audio track.".into(), true)),
            }
        }

        if let Some(note) = note {
            ui.label(egui::RichText::new(note).weak().size(11.0));
        }
        if let Some((text, is_error)) = self.notice.clone() {
            let colour = if is_error { BAD } else { GOOD };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(text).size(11.0).color(colour));
                if ui.small_button("×").clicked() {
                    self.notice = None;
                }
            });
        }
    }

    /// The zoomable timeline: ticks, in/out markers, playhead, click to seek.
    fn timeline(&mut self, ui: &mut egui::Ui, p: &mut Player, ctx: &egui::Context) {
        let duration = p.duration();
        if duration <= 0.0 {
            return;
        }
        if self.view_len <= 0.0 || self.view_len > duration {
            self.view_len = duration;
            self.view_start = 0.0;
        }
        self.view_start = self
            .view_start
            .clamp(0.0, (duration - self.view_len).max(0.0));

        let full_w = ui.available_width().max(80.0);
        let zoomed = self.view_len < duration - 1e-6;

        // When zoomed, a slim strip shows which slice of the whole clip is on
        // screen, so it stays obvious where you are.
        if zoomed {
            let (strip, _) = ui.allocate_exact_size(Vec2::new(full_w, 9.0), Sense::hover());
            let pa = ui.painter_at(strip);
            pa.rect_filled(strip, CornerRadius::same(2), Color32::from_rgb(32, 35, 42));
            let x0 = strip.left() + (self.view_start / duration) as f32 * strip.width();
            let x1 = strip.left()
                + ((self.view_start + self.view_len) / duration) as f32 * strip.width();
            pa.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x0, strip.top()),
                    Pos2::new(x1.max(x0 + 2.0), strip.bottom()),
                ),
                CornerRadius::same(2),
                Color32::from_rgb(64, 104, 156),
            );
            let px = strip.left() + (p.position() / duration) as f32 * strip.width();
            pa.line_segment(
                [Pos2::new(px, strip.top()), Pos2::new(px, strip.bottom())],
                Stroke::new(1.5, Color32::from_rgb(235, 238, 245)),
            );
            ui.add_space(3.0);
        }

        let (rect, resp) = ui.allocate_exact_size(Vec2::new(full_w, 42.0), Sense::click_and_drag());
        let (vs, vl) = (self.view_start, self.view_len);
        let t_to_x = |t: f64| rect.left() + ((t - vs) / vl) as f32 * rect.width();
        let x_to_t = |x: f32| vs + ((x - rect.left()) / rect.width().max(1.0)) as f64 * vl;

        let pa = ui.painter_at(rect);
        pa.rect(
            rect,
            CornerRadius::same(4),
            Color32::from_rgb(26, 29, 36),
            Stroke::new(1.0, Color32::from_rgb(58, 64, 76)),
            StrokeKind::Inside,
        );

        // Ticks at a round interval, chosen so roughly ten fit across.
        const STEPS: [f64; 14] = [
            0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0,
        ];
        let step = STEPS
            .iter()
            .copied()
            .find(|s| vl / s <= 10.0)
            .unwrap_or(3600.0);
        let mut t = (vs / step).ceil() * step;
        while t <= vs + vl {
            let x = t_to_x(t);
            pa.line_segment(
                [
                    Pos2::new(x, rect.bottom() - 11.0),
                    Pos2::new(x, rect.bottom()),
                ],
                Stroke::new(1.0, Color32::from_rgb(70, 77, 92)),
            );
            pa.text(
                Pos2::new(x + 3.0, rect.bottom() - 12.0),
                egui::Align2::LEFT_BOTTOM,
                player::timecode(t),
                egui::FontId::monospace(9.5),
                Color32::from_rgb(120, 128, 144),
            );
            t += step;
        }

        // The marked range, drawn under the playhead.
        {
            let (ma, mb) = (self.mark_in, self.mark_out);
            if ma.is_some() || mb.is_some() {
                let a = ma.unwrap_or(0.0);
                let b = mb.unwrap_or(duration);
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                let (x0, x1) = (t_to_x(lo).max(rect.left()), t_to_x(hi).min(rect.right()));
                if x1 > x0 {
                    pa.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(x0, rect.top() + 1.0),
                            Pos2::new(x1, rect.bottom() - 12.0),
                        ),
                        CornerRadius::ZERO,
                        Color32::from_rgba_premultiplied(20, 70, 45, 110),
                    );
                }
                for (mark, colour, flag_right) in [(ma, MARK_IN, true), (mb, MARK_OUT, false)] {
                    let Some(m) = mark else { continue };
                    let x = t_to_x(m);
                    if x < rect.left() - 1.0 || x > rect.right() + 1.0 {
                        continue;
                    }
                    pa.line_segment(
                        [
                            Pos2::new(x, rect.top() + 1.0),
                            Pos2::new(x, rect.bottom() - 1.0),
                        ],
                        Stroke::new(2.0, colour),
                    );
                    let dir = if flag_right { 9.0 } else { -9.0 };
                    pa.add(egui::Shape::convex_polygon(
                        vec![
                            Pos2::new(x, rect.top() + 1.0),
                            Pos2::new(x + dir, rect.top() + 1.0),
                            Pos2::new(x, rect.top() + 10.0),
                        ],
                        colour,
                        Stroke::NONE,
                    ));
                }
            }
        }

        let head = t_to_x(if self.scrubbing {
            self.scrub
        } else {
            p.position()
        });
        if head >= rect.left() - 1.0 && head <= rect.right() + 1.0 {
            pa.line_segment(
                [
                    Pos2::new(head, rect.top()),
                    Pos2::new(head, rect.bottom() - 1.0),
                ],
                Stroke::new(2.0, Color32::from_rgb(240, 243, 250)),
            );
            pa.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(head - 5.0, rect.top()),
                    Pos2::new(head + 5.0, rect.top()),
                    Pos2::new(head, rect.top() + 7.0),
                ],
                Color32::from_rgb(240, 243, 250),
                Stroke::NONE,
            ));
        }

        // Click or drag anywhere on the track to move the playhead, seeking
        // only once on release so one drag is one decoder restart.
        if let Some(pos) = resp.interact_pointer_pos() {
            self.scrub = x_to_t(pos.x).clamp(0.0, duration);
            self.scrubbing = true;
        }
        if resp.drag_stopped() || resp.clicked() {
            self.scrubbing = false;
            p.seek(self.scrub, ctx);
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.5 {
                let centre = ui
                    .input(|i| i.pointer.hover_pos())
                    .map(|hp| x_to_t(hp.x))
                    .unwrap_or_else(|| p.position());
                let factor = if scroll > 0.0 { 1.0 / 1.2 } else { 1.2 };
                self.zoom_timeline(factor, centre, duration);
            }
        }

        if zoomed {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "zoom {:.0}×  ·  + / - to zoom, scroll over the bar",
                        duration / self.view_len
                    ))
                    .weak()
                    .size(10.0),
                );
                if ui.small_button("Fit").clicked() {
                    self.view_len = duration;
                    self.view_start = 0.0;
                }
            });
        }
    }
}
