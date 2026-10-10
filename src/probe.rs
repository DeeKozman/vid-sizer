//! Reading media info out of the JSON that ffprobe emits.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::ffmpeg::{self, Tools};

#[derive(Deserialize)]
struct ProbeOut {
    #[serde(default)]
    streams: Vec<Stream>,
    #[serde(default)]
    format: Option<Format>,
}

#[derive(Deserialize)]
struct Format {
    #[serde(default)]
    format_long_name: Option<String>,
    #[serde(default)]
    format_name: Option<String>,
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    bit_rate: Option<String>,
    #[serde(default)]
    size: Option<String>,
}

#[derive(Deserialize)]
struct Stream {
    #[serde(default)]
    codec_type: Option<String>,
    #[serde(default)]
    codec_name: Option<String>,
    #[serde(default)]
    codec_long_name: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    pix_fmt: Option<String>,
    #[serde(default)]
    sample_aspect_ratio: Option<String>,
    #[serde(default)]
    display_aspect_ratio: Option<String>,
    #[serde(default)]
    r_frame_rate: Option<String>,
    #[serde(default)]
    avg_frame_rate: Option<String>,
    #[serde(default)]
    nb_frames: Option<String>,
    #[serde(default)]
    bit_rate: Option<String>,
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    channels: Option<u32>,
    #[serde(default)]
    channel_layout: Option<String>,
    #[serde(default)]
    sample_rate: Option<String>,
    #[serde(default)]
    tags: Option<HashMap<String, String>>,
    #[serde(default)]
    side_data_list: Vec<SideData>,
    #[serde(default)]
    disposition: Option<HashMap<String, i32>>,
}

#[derive(Deserialize)]
struct SideData {
    #[serde(default)]
    rotation: Option<f64>,
}

/// Everything the info panel shows, plus the numbers the resize math needs.
#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub path: PathBuf,
    pub container: String,
    pub size_bytes: Option<u64>,
    pub duration: Option<f64>,
    pub bit_rate: Option<u64>,

    pub v_codec: String,
    pub v_profile: Option<String>,
    /// Dimensions as stored in the file.
    pub stored_width: u32,
    pub stored_height: u32,
    /// Dimensions as the frame is actually presented, after rotation metadata
    /// is applied. All resize and crop math uses these, because ffmpeg
    /// auto-rotates before our filters run.
    pub width: u32,
    pub height: u32,
    pub rotation: i32,
    pub pix_fmt: Option<String>,
    pub sar: Option<String>,
    pub dar: Option<String>,
    pub fps: Option<f64>,
    pub frames: Option<u64>,
    pub v_bit_rate: Option<u64>,

    pub a_codec: Option<String>,
    pub a_channels: Option<u32>,
    pub a_layout: Option<String>,
    pub a_sample_rate: Option<u32>,
    pub a_bit_rate: Option<u64>,
    pub has_audio: bool,
}

impl MediaInfo {
    pub fn aspect(&self) -> f64 {
        if self.height == 0 {
            1.0
        } else {
            self.width as f64 / self.height as f64
        }
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

fn parse_rate(s: &str) -> Option<f64> {
    let (num, den) = s.split_once('/')?;
    let num: f64 = num.trim().parse().ok()?;
    let den: f64 = den.trim().parse().ok()?;
    if den == 0.0 || num == 0.0 {
        None
    } else {
        Some(num / den)
    }
}

/// ffprobe reports rotation either as stream side data (a negative angle, the
/// modern way) or as a `rotate` tag (older files). Normalise to 0/90/180/270
/// clockwise.
fn stream_rotation(s: &Stream) -> i32 {
    let raw = s
        .side_data_list
        .iter()
        .find_map(|sd| sd.rotation)
        .or_else(|| {
            s.tags
                .as_ref()
                .and_then(|t| {
                    t.iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("rotate"))
                        .map(|(_, v)| v.clone())
                })
                .and_then(|v| v.trim().parse::<f64>().ok())
                .map(|deg| -deg)
        })
        .unwrap_or(0.0);

    let deg = (-raw).rem_euclid(360.0).round() as i32;
    match deg {
        90 | 180 | 270 => deg,
        _ => 0,
    }
}

pub fn probe(tools: &Tools, path: &Path) -> Result<MediaInfo, String> {
    let out = ffmpeg::command(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("could not run ffprobe: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(if err.is_empty() {
            "ffprobe failed: unsupported or unreadable file".to_owned()
        } else {
            err
        });
    }

    let parsed: ProbeOut = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("could not parse ffprobe output: {e}"))?;

    // Skip cover art, which shows up as a video stream with an attached_pic
    // disposition and would otherwise be mistaken for the real picture.
    let video = parsed
        .streams
        .iter()
        .find(|s| {
            s.codec_type.as_deref() == Some("video")
                && s.disposition
                    .as_ref()
                    .and_then(|d| d.get("attached_pic"))
                    .copied()
                    .unwrap_or(0)
                    == 0
                && s.width.unwrap_or(0) > 0
                && s.height.unwrap_or(0) > 0
        })
        .ok_or_else(|| "no video stream in this file".to_owned())?;

    let audio = parsed
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("audio"));

    let stored_width = video.width.unwrap_or(0);
    let stored_height = video.height.unwrap_or(0);
    let rotation = stream_rotation(video);
    let (width, height) = if rotation == 90 || rotation == 270 {
        (stored_height, stored_width)
    } else {
        (stored_width, stored_height)
    };

    let fmt = parsed.format.as_ref();
    let container = fmt
        .and_then(|f| f.format_long_name.clone().or_else(|| f.format_name.clone()))
        .unwrap_or_else(|| "unknown".to_owned());

    let duration = fmt
        .and_then(|f| f.duration.as_ref())
        .and_then(|d| d.parse::<f64>().ok())
        .or_else(|| video.duration.as_ref().and_then(|d| d.parse::<f64>().ok()))
        .filter(|d| *d > 0.0);

    let fps = video
        .r_frame_rate
        .as_deref()
        .and_then(parse_rate)
        .or_else(|| video.avg_frame_rate.as_deref().and_then(parse_rate));

    Ok(MediaInfo {
        path: path.to_path_buf(),
        container,
        size_bytes: fmt
            .and_then(|f| f.size.as_ref())
            .and_then(|s| s.parse().ok())
            .or_else(|| std::fs::metadata(path).ok().map(|m| m.len())),
        duration,
        bit_rate: fmt
            .and_then(|f| f.bit_rate.as_ref())
            .and_then(|s| s.parse().ok()),

        v_codec: video
            .codec_long_name
            .clone()
            .or_else(|| video.codec_name.clone())
            .unwrap_or_else(|| "unknown".to_owned()),
        v_profile: video.profile.clone(),
        stored_width,
        stored_height,
        width,
        height,
        rotation,
        pix_fmt: video.pix_fmt.clone(),
        sar: video.sample_aspect_ratio.clone(),
        dar: video.display_aspect_ratio.clone(),
        fps,
        frames: video.nb_frames.as_ref().and_then(|s| s.parse().ok()),
        v_bit_rate: video.bit_rate.as_ref().and_then(|s| s.parse().ok()),

        a_codec: audio.and_then(|a| a.codec_long_name.clone().or_else(|| a.codec_name.clone())),
        a_channels: audio.and_then(|a| a.channels),
        a_layout: audio.and_then(|a| a.channel_layout.clone()),
        a_sample_rate: audio.and_then(|a| a.sample_rate.as_ref().and_then(|s| s.parse().ok())),
        a_bit_rate: audio.and_then(|a| a.bit_rate.as_ref().and_then(|s| s.parse().ok())),
        has_audio: audio.is_some(),
    })
}
