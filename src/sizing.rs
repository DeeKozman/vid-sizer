//! Target-size presets, even-dimension snapping, crop geometry and output naming.

use std::path::{Path, PathBuf};

/// Aspect ratios within this much of each other count as a match, so a 1.7777
/// source going to 1920x1080 does not arm a pointless 1-pixel crop.
const ASPECT_EPSILON: f64 = 0.002;

pub struct Preset {
    pub label: &'static str,
    pub width: u32,
    pub height: u32,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        label: "4K UHD  3840 x 2160",
        width: 3840,
        height: 2160,
    },
    Preset {
        label: "1440p  2560 x 1440",
        width: 2560,
        height: 1440,
    },
    Preset {
        label: "1080p  1920 x 1080",
        width: 1920,
        height: 1080,
    },
    Preset {
        label: "720p  1280 x 720",
        width: 1280,
        height: 720,
    },
    Preset {
        label: "480p  854 x 480",
        width: 854,
        height: 480,
    },
    Preset {
        label: "360p  640 x 360",
        width: 640,
        height: 360,
    },
    Preset {
        label: "Vertical 9:16  1080 x 1920",
        width: 1080,
        height: 1920,
    },
    Preset {
        label: "Vertical 9:16  720 x 1280",
        width: 720,
        height: 1280,
    },
    Preset {
        label: "Square 1:1  1080 x 1080",
        width: 1080,
        height: 1080,
    },
    Preset {
        label: "Portrait 4:5  1080 x 1350",
        width: 1080,
        height: 1350,
    },
    Preset {
        label: "Cinema 2.39:1  1920 x 804",
        width: 1920,
        height: 804,
    },
];

/// H.264 with 4:2:0 chroma needs even dimensions, so every number we hand
/// ffmpeg gets snapped. Rounds to nearest, with a floor of 2.
pub fn even_round(v: f64) -> u32 {
    let snapped = (v / 2.0).round() * 2.0;
    snapped.max(2.0) as u32
}

/// Like [`even_round`] but never rounds up, for crop sizes and offsets that
/// must stay inside the source frame.
pub fn even_floor(v: f64) -> u32 {
    let snapped = (v / 2.0).floor() * 2.0;
    snapped.max(2.0) as u32
}

/// Like [`even_floor`] but allows zero, for crop offsets that legitimately
/// sit flush against the left or top edge. Chroma subsampling wants these even
/// too.
pub fn even_down(v: f64) -> u32 {
    let snapped = (v / 2.0).floor() * 2.0;
    snapped.max(0.0) as u32
}

/// The largest rect of aspect `tw:th` that fits inside `sw` x `sh`.
pub fn max_crop(sw: u32, sh: u32, tw: u32, th: u32) -> (u32, u32) {
    if sw == 0 || sh == 0 || tw == 0 || th == 0 {
        return (sw.max(2), sh.max(2));
    }
    let src_aspect = sw as f64 / sh as f64;
    let tgt_aspect = tw as f64 / th as f64;

    if src_aspect > tgt_aspect {
        // Source is wider than the target: keep full height, trim the sides.
        let cw = even_floor(sh as f64 * tgt_aspect).min(even_floor(sw as f64));
        (cw, even_floor(sh as f64))
    } else {
        // Source is taller: keep full width, trim top and bottom.
        let ch = even_floor(sw as f64 / tgt_aspect).min(even_floor(sh as f64));
        (even_floor(sw as f64), ch)
    }
}

/// Does going from this source to this target need a crop, or do the aspects
/// already agree?
pub fn needs_crop(sw: u32, sh: u32, tw: u32, th: u32) -> bool {
    if sw == 0 || sh == 0 || tw == 0 || th == 0 {
        return false;
    }
    let src = sw as f64 / sh as f64;
    let tgt = tw as f64 / th as f64;
    (src - tgt).abs() > ASPECT_EPSILON
}

/// Reduce `w:h` to a readable ratio like `16:9`.
pub fn ratio_label(w: u32, h: u32) -> String {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    if w == 0 || h == 0 {
        return "-".to_owned();
    }
    let g = gcd(w, h);
    let (rw, rh) = (w / g, h / g);
    if rw <= 64 && rh <= 64 {
        format!("{rw}:{rh}")
    } else {
        format!("{:.3}:1", w as f64 / h as f64)
    }
}

/// libx264 cannot go into a WebM container, so those outputs become MP4.
/// Everything else keeps the source extension.
pub fn output_extension(src: &Path) -> String {
    let ext = src
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| "mp4".to_owned());

    match ext.as_str() {
        "webm" | "ogv" | "ogg" => "mp4".to_owned(),
        other => other.to_owned(),
    }
}

/// `clip.mp4` plus 1920x1080 becomes `clip_1920x1080.mp4` in the same folder,
/// with a numeric suffix if that name is already taken.
pub fn output_path(src: &Path, width: u32, height: u32) -> PathBuf {
    let dir = src.parent().unwrap_or_else(|| Path::new("."));
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_owned());
    let ext = output_extension(src);

    let base = format!("{stem}_{width}x{height}");
    let first = dir.join(format!("{base}.{ext}"));
    if !first.exists() {
        return first;
    }
    for n in 2..10_000 {
        let candidate = dir.join(format!("{base}_{n}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

/// A timestamp as `HH-MM-SS-mmm`. Every field is zero padded to a fixed
/// width, so sorting these strings sorts the moments they came from.
pub fn timecode_filename(secs: f64) -> String {
    let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
    let millis_total = (secs * 1000.0).round() as u64;
    let ms = millis_total % 1000;
    let total = millis_total / 1000;
    let h = (total / 3600).min(99);
    let m = (total % 3600) / 60;
    let s = total % 60;
    format!("{h:02}-{m:02}-{s:02}-{ms:03}")
}

/// A short, filesystem-safe stub of a file name, stable for a given input.
pub fn abbreviate(stem: &str, max: usize) -> String {
    let mut out = String::new();
    let mut last_sep = false;
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_sep = false;
        } else if !last_sep && !out.is_empty() {
            out.push('_');
            last_sep = true;
        }
        if out.len() >= max {
            break;
        }
    }
    let trimmed = out.trim_end_matches('_').to_owned();
    if trimmed.is_empty() {
        "clip".to_owned()
    } else {
        trimmed
    }
}

pub fn file_stem(src: &Path) -> String {
    src.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".to_owned())
}

/// Screenshots for a clip live in a folder named after it, beside it.
pub fn shot_folder(src: &Path) -> PathBuf {
    src.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(file_stem(src))
}

/// `<folder>/<abbrev>_<HH-MM-SS-mmm>.png`. The same moment in the same clip
/// always produces the same path, so a repeated grab overwrites rather than
/// piling up, and a directory listing is in timeline order.
pub fn shot_path(src: &Path, secs: f64) -> PathBuf {
    let name = format!(
        "{}_{}.png",
        abbreviate(&file_stem(src), 14),
        timecode_filename(secs)
    );
    shot_folder(src).join(name)
}

/// `<stem>_<in>-<out>_<W>x<H>.<ext>` beside the source, with the same
/// collision suffix as a full render.
pub fn trim_output_path(src: &Path, width: u32, height: u32, start: f64, end: f64) -> PathBuf {
    let dir = src.parent().unwrap_or_else(|| Path::new("."));
    let ext = output_extension(src);
    let base = format!(
        "{}_{}-{}_{}x{}",
        file_stem(src),
        timecode_filename(start),
        timecode_filename(end),
        width,
        height
    );

    let first = dir.join(format!("{base}.{ext}"));
    if !first.exists() {
        return first;
    }
    for n in 2..10_000 {
        let candidate = dir.join(format!("{base}_{n}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

/// Smallest crop the handles will let you make, in source pixels.
pub const MIN_CROP: f64 = 16.0;

/// A crop rectangle in source pixels, kept as floats while it is being
/// dragged so the motion stays smooth; it is snapped to even integers only
/// when handed to ffmpeg.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Which part of the crop box a drag is moving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Move,
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
}

impl Handle {
    fn moves_west(self) -> bool {
        matches!(self, Handle::W | Handle::NW | Handle::SW)
    }
    fn moves_east(self) -> bool {
        matches!(self, Handle::E | Handle::NE | Handle::SE)
    }
    fn moves_north(self) -> bool {
        matches!(self, Handle::N | Handle::NW | Handle::NE)
    }
    fn moves_south(self) -> bool {
        matches!(self, Handle::S | Handle::SW | Handle::SE)
    }
}

impl CropRect {
    /// A box of `w` x `h` sitting in the middle of a `bw` x `bh` frame.
    pub fn centred(bw: f64, bh: f64, w: f64, h: f64) -> Self {
        let w = w.clamp(MIN_CROP, bw.max(MIN_CROP));
        let h = h.clamp(MIN_CROP, bh.max(MIN_CROP));
        Self {
            x: ((bw - w) * 0.5).max(0.0),
            y: ((bh - h) * 0.5).max(0.0),
            w,
            h,
        }
    }

    pub fn aspect(&self) -> f64 {
        if self.h <= 0.0 { 1.0 } else { self.w / self.h }
    }

    /// Resize about the centre, used by the width and height number boxes.
    pub fn resized(&self, w: f64, h: f64, bw: f64, bh: f64) -> Self {
        let w = w.clamp(MIN_CROP, bw.max(MIN_CROP));
        let h = h.clamp(MIN_CROP, bh.max(MIN_CROP));
        let cx = self.x + self.w * 0.5;
        let cy = self.y + self.h * 0.5;
        Self {
            x: (cx - w * 0.5).clamp(0.0, (bw - w).max(0.0)),
            y: (cy - h * 0.5).clamp(0.0, (bh - h).max(0.0)),
            w,
            h,
        }
    }

    /// Even x, y, width and height inside a `bw` x `bh` frame, which is what
    /// ffmpeg's crop filter and 4:2:0 chroma need.
    pub fn to_even(&self, bw: u32, bh: u32) -> (u32, u32, u32, u32) {
        let w = even_floor(self.w.max(2.0)).min(even_floor(bw.max(2) as f64));
        let h = even_floor(self.h.max(2.0)).min(even_floor(bh.max(2) as f64));
        let x = even_down(self.x.max(0.0)).min(bw.saturating_sub(w)) / 2 * 2;
        let y = even_down(self.y.max(0.0)).min(bh.saturating_sub(h)) / 2 * 2;
        (x, y, w, h)
    }
}

/// Apply a drag to the box as it was when the drag began. Working from the
/// starting rect and a total delta, rather than accumulating per frame, keeps
/// the box from creeping when the pointer is clamped at an edge.
///
/// `lock` is the aspect ratio to hold, if any.
pub fn drag_crop(
    start: CropRect,
    handle: Handle,
    dx: f64,
    dy: f64,
    bw: f64,
    bh: f64,
    lock: Option<f64>,
) -> CropRect {
    if handle == Handle::Move {
        return CropRect {
            x: (start.x + dx).clamp(0.0, (bw - start.w).max(0.0)),
            y: (start.y + dy).clamp(0.0, (bh - start.h).max(0.0)),
            ..start
        };
    }

    // The point that stays put while this handle moves: the opposite corner,
    // or the opposite edge's midpoint.
    let anchor_x = if handle.moves_west() {
        start.x + start.w
    } else if handle.moves_east() {
        start.x
    } else {
        start.x + start.w * 0.5
    };
    let anchor_y = if handle.moves_north() {
        start.y + start.h
    } else if handle.moves_south() {
        start.y
    } else {
        start.y + start.h * 0.5
    };

    let mut w = if handle.moves_west() {
        start.w - dx
    } else if handle.moves_east() {
        start.w + dx
    } else {
        start.w
    };
    let mut h = if handle.moves_north() {
        start.h - dy
    } else if handle.moves_south() {
        start.h + dy
    } else {
        start.h
    };

    match lock.filter(|a| *a > 0.0 && a.is_finite()) {
        Some(a) => {
            // Height is always re-derived from width just below, so the only
            // question here is which way the drag set the width. On a corner
            // that is whichever way the pointer actually moved further.
            let height_leads = match handle {
                Handle::N | Handle::S => true,
                Handle::E | Handle::W => false,
                _ => dy.abs() > dx.abs(),
            };
            if height_leads {
                w = h * a;
            }
            // Cap to the largest box of this aspect the frame can hold, so
            // the ratio survives the clamp instead of being squashed by it.
            let max_w = bw.min(bh * a).max(MIN_CROP);
            w = w.clamp(MIN_CROP, max_w);
            h = (w / a).max(MIN_CROP);
            w = h * a;
        }
        None => {
            w = w.clamp(MIN_CROP, bw.max(MIN_CROP));
            h = h.clamp(MIN_CROP, bh.max(MIN_CROP));
        }
    }

    let x = if handle.moves_west() {
        anchor_x - w
    } else if handle.moves_east() {
        anchor_x
    } else {
        anchor_x - w * 0.5
    };
    let y = if handle.moves_north() {
        anchor_y - h
    } else if handle.moves_south() {
        anchor_y
    } else {
        anchor_y - h * 0.5
    };

    CropRect {
        x: x.clamp(0.0, (bw - w).max(0.0)),
        y: y.clamp(0.0, (bh - h).max(0.0)),
        w,
        h,
    }
}

/// `<stem>.mp3`, or `<stem>_<in>-<out>.mp3` when only a range is wanted.
pub fn mp3_output_path(src: &Path, range: Option<(f64, f64)>) -> PathBuf {
    let dir = src.parent().unwrap_or_else(|| Path::new("."));
    let base = match range {
        Some((a, b)) => format!(
            "{}_{}-{}",
            file_stem(src),
            timecode_filename(a),
            timecode_filename(b)
        ),
        None => file_stem(src),
    };

    let first = dir.join(format!("{base}.mp3"));
    if !first.exists() {
        return first;
    }
    for n in 2..10_000 {
        let candidate = dir.join(format!("{base}_{n}.mp3"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

pub fn human_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "-".to_owned();
    }
    let total = secs.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    let frac = ((secs - total as f64).abs() * 100.0).round() as u64;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}.{frac:02}")
    }
}

pub fn human_bitrate(bps: u64) -> String {
    if bps >= 1_000_000 {
        format!("{:.2} Mb/s", bps as f64 / 1_000_000.0)
    } else {
        format!("{} kb/s", bps / 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn even_snapping_stays_in_bounds() {
        assert_eq!(even_round(1079.0), 1080);
        assert_eq!(even_floor(1079.0), 1078);
        assert_eq!(even_floor(1.0), 2);
    }

    #[test]
    fn wide_source_to_square_trims_sides() {
        // 1920x1080 into 1:1 keeps the full height.
        let (cw, ch) = max_crop(1920, 1080, 1080, 1080);
        assert_eq!((cw, ch), (1080, 1080));
    }

    #[test]
    fn tall_source_to_wide_trims_top_and_bottom() {
        // 1080x1920 into 16:9 keeps the full width.
        let (cw, ch) = max_crop(1080, 1920, 1920, 1080);
        assert_eq!((cw, ch), (1080, 606));
        assert!(ch <= 1920);
    }

    #[test]
    fn crop_never_exceeds_source() {
        for (sw, sh) in [(1920u32, 1080u32), (640, 480), (1080, 1920), (3840, 2160)] {
            for p in PRESETS {
                let (cw, ch) = max_crop(sw, sh, p.width, p.height);
                assert!(cw <= sw, "{cw} > {sw}");
                assert!(ch <= sh, "{ch} > {sh}");
                assert_eq!(cw % 2, 0);
                assert_eq!(ch % 2, 0);
            }
        }
    }

    #[test]
    fn matching_aspect_needs_no_crop() {
        assert!(!needs_crop(3840, 2160, 1920, 1080));
        assert!(needs_crop(1920, 1080, 1080, 1080));
    }

    #[test]
    fn timecodes_sort_in_timeline_order() {
        assert_eq!(timecode_filename(0.0), "00-00-00-000");
        assert_eq!(timecode_filename(1.5), "00-00-01-500");
        assert_eq!(timecode_filename(83.456), "00-01-23-456");
        assert_eq!(timecode_filename(3661.0), "01-01-01-000");

        // The whole point: lexical order has to match chronological order.
        let times = [0.0, 0.5, 9.9, 10.0, 59.999, 60.0, 599.0, 3600.0, 7322.25];
        let mut names: Vec<String> = times.iter().map(|t| timecode_filename(*t)).collect();
        let sorted = {
            let mut c = names.clone();
            c.sort();
            c
        };
        assert_eq!(names, sorted, "names must already be in order");
        names.reverse();
        names.sort();
        assert_eq!(names[0], timecode_filename(0.0));
    }

    #[test]
    fn abbreviation_is_short_safe_and_stable() {
        assert_eq!(abbreviate("My Clip 01", 14), "My_Clip_01");
        assert_eq!(abbreviate("a/b\\c:d", 14), "a_b_c_d");
        assert_eq!(abbreviate("!!!", 14), "clip");
        assert_eq!(abbreviate("", 14), "clip");
        let long = abbreviate("averyveryverylongfilename", 14);
        assert!(long.len() <= 14, "{long}");
        // Same input, same output, every time.
        assert_eq!(abbreviate("My Clip 01", 14), abbreviate("My Clip 01", 14));
    }

    #[test]
    fn shots_land_in_a_folder_named_after_the_clip() {
        let src = Path::new("D:/clips/Holiday Reel.mp4");
        assert_eq!(shot_folder(src), Path::new("D:/clips/Holiday Reel"));
        assert_eq!(
            shot_path(src, 83.456).file_name().unwrap(),
            "Holiday_Reel_00-01-23-456.png"
        );
        // Same moment, same name, so a second grab replaces the first.
        assert_eq!(shot_path(src, 83.456), shot_path(src, 83.456));
        assert_ne!(shot_path(src, 83.456), shot_path(src, 83.457));
    }

    /// Every drag, locked or not, has to leave a box that still fits.
    fn assert_sane(r: CropRect, bw: f64, bh: f64) {
        assert!(r.x >= -1e-6, "x {} went negative", r.x);
        assert!(r.y >= -1e-6, "y {} went negative", r.y);
        assert!(r.w >= MIN_CROP - 1e-6, "w {} below the minimum", r.w);
        assert!(r.h >= MIN_CROP - 1e-6, "h {} below the minimum", r.h);
        assert!(r.x + r.w <= bw + 1e-6, "right edge {} past {bw}", r.x + r.w);
        assert!(
            r.y + r.h <= bh + 1e-6,
            "bottom edge {} past {bh}",
            r.y + r.h
        );
    }

    #[test]
    fn dragging_a_corner_resizes_from_the_opposite_one() {
        let start = CropRect {
            x: 100.0,
            y: 100.0,
            w: 400.0,
            h: 300.0,
        };
        // Pull the south-east corner out by 50 in both directions.
        let r = drag_crop(start, Handle::SE, 50.0, 50.0, 1920.0, 1080.0, None);
        assert_eq!((r.x, r.y), (100.0, 100.0), "the far corner must not move");
        assert_eq!((r.w, r.h), (450.0, 350.0));

        // Pull the north-west corner in; the south-east corner holds.
        let r = drag_crop(start, Handle::NW, 40.0, 30.0, 1920.0, 1080.0, None);
        assert_eq!((r.x, r.y), (140.0, 130.0));
        assert_eq!((r.x + r.w, r.y + r.h), (500.0, 400.0));
    }

    #[test]
    fn moving_slides_without_resizing_and_stops_at_the_edge() {
        let start = CropRect {
            x: 100.0,
            y: 100.0,
            w: 400.0,
            h: 300.0,
        };
        let r = drag_crop(start, Handle::Move, -30.0, 25.0, 1920.0, 1080.0, None);
        assert_eq!((r.w, r.h), (400.0, 300.0), "a move must not resize");
        assert_eq!((r.x, r.y), (70.0, 125.0));

        // Shoved hard into the corner it stops flush, not past.
        let r = drag_crop(start, Handle::Move, -9999.0, -9999.0, 1920.0, 1080.0, None);
        assert_eq!((r.x, r.y), (0.0, 0.0));
        let r = drag_crop(start, Handle::Move, 9999.0, 9999.0, 1920.0, 1080.0, None);
        assert_eq!((r.x + r.w, r.y + r.h), (1920.0, 1080.0));
        assert_sane(r, 1920.0, 1080.0);
    }

    #[test]
    fn locked_aspect_survives_every_handle_and_the_frame_edges() {
        let start = CropRect {
            x: 400.0,
            y: 300.0,
            w: 400.0,
            h: 200.0,
        };
        let locked = start.aspect(); // 2.0

        let handles = [
            Handle::N,
            Handle::S,
            Handle::E,
            Handle::W,
            Handle::NE,
            Handle::NW,
            Handle::SE,
            Handle::SW,
        ];
        // Including deltas far larger than the frame, which is what a fast
        // drag off the edge of the window actually produces.
        for d in [-5000.0, -300.0, -40.0, 40.0, 300.0, 5000.0] {
            for h in handles {
                let r = drag_crop(start, h, d, d * 0.5, 1920.0, 1080.0, Some(locked));
                assert_sane(r, 1920.0, 1080.0);
                assert!(
                    (r.aspect() - locked).abs() < 1e-6,
                    "{h:?} with delta {d} gave aspect {} instead of {locked}",
                    r.aspect()
                );
            }
        }
    }

    #[test]
    fn unlocked_drags_stay_inside_the_frame() {
        let start = CropRect {
            x: 10.0,
            y: 10.0,
            w: 100.0,
            h: 80.0,
        };
        for d in [-9999.0, -50.0, 50.0, 9999.0] {
            for h in [
                Handle::N,
                Handle::S,
                Handle::E,
                Handle::W,
                Handle::NE,
                Handle::NW,
                Handle::SE,
                Handle::SW,
                Handle::Move,
            ] {
                assert_sane(drag_crop(start, h, d, -d, 640.0, 480.0, None), 640.0, 480.0);
            }
        }
    }

    #[test]
    fn even_snapping_keeps_the_box_inside() {
        // Odd numbers everywhere, flush against the bottom right corner.
        let r = CropRect {
            x: 101.0,
            y: 77.0,
            w: 333.0,
            h: 211.0,
        };
        let (x, y, w, h) = r.to_even(640, 480);
        for v in [x, y, w, h] {
            assert_eq!(v % 2, 0, "{v} is odd");
        }
        assert!(x + w <= 640 && y + h <= 480);

        let flush = CropRect {
            x: 639.0,
            y: 479.0,
            w: 999.0,
            h: 999.0,
        };
        let (x, y, w, h) = flush.to_even(640, 480);
        assert!(x + w <= 640 && y + h <= 480, "{x}+{w} / {y}+{h}");
    }

    #[test]
    fn resizing_by_number_keeps_the_centre() {
        let start = CropRect {
            x: 100.0,
            y: 100.0,
            w: 200.0,
            h: 200.0,
        };
        let r = start.resized(100.0, 100.0, 1920.0, 1080.0);
        assert_eq!((r.x, r.y), (150.0, 150.0), "centre should hold");
        // Oversized requests are capped to the frame.
        let r = start.resized(99999.0, 99999.0, 640.0, 480.0);
        assert_eq!((r.w, r.h), (640.0, 480.0));
        assert_sane(r, 640.0, 480.0);
    }

    #[test]
    fn mp3_names_say_the_range_only_when_there_is_one() {
        let src = Path::new("D:/clips/Talk.mkv");
        assert_eq!(mp3_output_path(src, None).file_name().unwrap(), "Talk.mp3");
        assert_eq!(
            mp3_output_path(src, Some((1.5, 95.25)))
                .file_name()
                .unwrap(),
            "Talk_00-00-01-500-00-01-35-250.mp3"
        );
    }

    #[test]
    fn webm_falls_back_to_mp4() {
        assert_eq!(output_extension(Path::new("a.webm")), "mp4");
        assert_eq!(output_extension(Path::new("a.MOV")), "mov");
    }
}
