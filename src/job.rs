//! Running ffmpeg: poster-frame extraction and the encode job with progress.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use crate::ffmpeg::{self, Tools};

/// How the crop rectangle was resolved, or `None` when aspects already match.
#[derive(Debug, Clone, Copy)]
pub struct Crop {
    pub w: u32,
    pub h: u32,
    pub x: u32,
    pub y: u32,
}

#[derive(Debug, Clone)]
pub struct EncodeSpec {
    pub input: PathBuf,
    pub output: PathBuf,
    pub crop: Option<Crop>,
    pub width: u32,
    pub height: u32,
    pub crf: u32,
    pub preset: String,
    pub duration: Option<f64>,
    pub copy_audio: bool,
    /// Export only this `(start, end)` slice of the source.
    pub trim: Option<(f64, f64)>,
    /// Remux without re-encoding. Much faster, but cuts land on the nearest
    /// keyframe and no crop or scale can be applied.
    pub stream_copy: bool,
    /// Rip the audio to MP3 at this bitrate instead of writing video.
    pub mp3_kbps: Option<u32>,
}

impl EncodeSpec {
    /// How many seconds this job will actually write, for the progress bar.
    pub fn work_seconds(&self) -> Option<f64> {
        match self.trim {
            Some((a, b)) if b > a => Some(b - a),
            _ => self.duration,
        }
    }
}

impl EncodeSpec {
    /// The `-vf` chain: crop first so we only scale the pixels we keep.
    pub fn filter(&self) -> String {
        let scale = format!(
            "scale={}:{}:flags=lanczos,setsar=1",
            self.width, self.height
        );
        match self.crop {
            Some(c) => format!("crop={}:{}:{}:{},{scale}", c.w, c.h, c.x, c.y),
            None => scale,
        }
    }

    /// The full argument list, also used to show the command in the UI.
    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec!["-hide_banner".into(), "-nostdin".into(), "-y".into()];

        // Seeking before -i is the fast path and rebases timestamps to zero,
        // so the length is given as a duration rather than an end point.
        if let Some((start, _)) = self.trim {
            a.push("-ss".into());
            a.push(format!("{start:.3}"));
        }
        a.push("-i".into());
        a.push(self.input.to_string_lossy().into_owned());
        if let Some((start, end)) = self.trim {
            a.push("-t".into());
            a.push(format!("{:.3}", (end - start).max(0.01)));
        }

        // Ripping audio: no video stream, no filters, nothing else to decide.
        if let Some(kbps) = self.mp3_kbps {
            a.extend([
                "-vn".into(),
                "-map".into(),
                "0:a:0".into(),
                "-c:a".into(),
                "libmp3lame".into(),
                "-b:a".into(),
                format!("{kbps}k"),
                "-progress".into(),
                "pipe:1".into(),
                "-nostats".into(),
                self.output.to_string_lossy().into_owned(),
            ]);
            return a;
        }

        // Take only the first video and, if present, the first audio stream.
        // Without this, cover art or extra tracks derail the map.
        a.push("-map".into());
        a.push("0:v:0".into());
        if self.copy_audio {
            a.push("-map".into());
            a.push("0:a:0?".into());
        }

        if self.stream_copy {
            a.push("-c".into());
            a.push("copy".into());
            // A keyframe cut can start with negative timestamps; rebase them
            // or players open the clip at the wrong spot.
            a.push("-avoid_negative_ts".into());
            a.push("make_zero".into());
        } else {
            a.push("-vf".into());
            a.push(self.filter());
            a.extend([
                "-c:v".into(),
                "libx264".into(),
                "-crf".into(),
                self.crf.to_string(),
                "-preset".into(),
                self.preset.clone(),
                "-pix_fmt".into(),
                "yuv420p".into(),
            ]);
            if self.copy_audio {
                a.push("-c:a".into());
                a.push("copy".into());
            } else {
                a.push("-an".into());
            }
        }

        let ext = self
            .output
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if matches!(ext.as_str(), "mp4" | "mov" | "m4v") {
            a.push("-movflags".into());
            a.push("+faststart".into());
        }

        a.extend([
            "-progress".into(),
            "pipe:1".into(),
            "-nostats".into(),
            self.output.to_string_lossy().into_owned(),
        ]);
        a
    }

    pub fn preview_command(&self, tools: &Tools) -> String {
        let mut s = format!("\"{}\"", tools.ffmpeg.display());
        for arg in self.args() {
            if arg.contains(' ') || arg.contains('\\') {
                s.push_str(&format!(" \"{arg}\""));
            } else {
                s.push(' ');
                s.push_str(&arg);
            }
        }
        s
    }
}

/// Messages the encode thread sends back to the UI.
pub enum JobMsg {
    Progress {
        fraction: f32,
        speed: Option<String>,
    },
    Done(PathBuf),
    Failed(String),
    Cancelled,
}

/// A running encode. Dropping this does not stop ffmpeg; call [`Job::cancel`].
pub struct Job {
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<Mutex<bool>>,
}

impl Job {
    pub fn cancel(&self) {
        *self.cancelled.lock().unwrap() = true;
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }
}

/// Start the encode on a worker thread. Every message is followed by a repaint
/// request so the progress bar moves without the window needing focus.
pub fn spawn(
    tools: &Tools,
    spec: EncodeSpec,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
) -> Result<Job, String> {
    let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let cancelled = Arc::new(Mutex::new(false));

    let mut child = ffmpeg::command(&tools.ffmpeg)
        .args(spec.args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;

    let stdout = child.stdout.take().ok_or("no ffmpeg stdout")?;
    let stderr = child.stderr.take().ok_or("no ffmpeg stderr")?;

    // ffmpeg writes its real diagnostics to stderr; keep the tail so a failure
    // can say why instead of just reporting an exit code.
    let err_tail = Arc::new(Mutex::new(Vec::<String>::new()));
    {
        let err_tail = Arc::clone(&err_tail);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut t = err_tail.lock().unwrap();
                t.push(line);
                if t.len() > 40 {
                    t.remove(0);
                }
            }
        });
    }

    *child_slot.lock().unwrap() = Some(child);

    let job = Job {
        child: Arc::clone(&child_slot),
        cancelled: Arc::clone(&cancelled),
    };

    let output = spec.output.clone();
    // Progress is measured against what this job writes, which is the trim
    // length when there is one, not the length of the source.
    let duration = spec.work_seconds();

    std::thread::spawn(move || {
        let mut speed: Option<String> = None;

        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();

            match key.trim() {
                "speed" => {
                    if value != "N/A" {
                        speed = Some(value.to_owned());
                    }
                }
                "out_time_us" | "out_time_ms" => {
                    // The key is a misnomer in older builds: both report
                    // microseconds.
                    if let (Ok(us), Some(total)) = (value.parse::<f64>(), duration) {
                        if total > 0.0 {
                            let fraction = ((us / 1_000_000.0) / total).clamp(0.0, 1.0) as f32;
                            let _ = tx.send(JobMsg::Progress {
                                fraction,
                                speed: speed.clone(),
                            });
                            ctx.request_repaint();
                        }
                    }
                }
                _ => {}
            }
        }

        let status = {
            let mut slot = child_slot.lock().unwrap();
            match slot.as_mut() {
                Some(c) => c.wait(),
                None => return,
            }
        };

        let was_cancelled = *cancelled.lock().unwrap();
        let msg = if was_cancelled {
            // A killed ffmpeg leaves a truncated file behind.
            let _ = std::fs::remove_file(&output);
            JobMsg::Cancelled
        } else {
            match status {
                Ok(s) if s.success() => JobMsg::Done(output.clone()),
                Ok(s) => {
                    let _ = std::fs::remove_file(&output);
                    let tail = err_tail.lock().unwrap().join("\n");
                    let detail = if tail.trim().is_empty() {
                        format!("ffmpeg exited with {s}")
                    } else {
                        tail
                    };
                    JobMsg::Failed(detail)
                }
                Err(e) => JobMsg::Failed(format!("waiting on ffmpeg failed: {e}")),
            }
        };

        let _ = tx.send(msg);
        ctx.request_repaint();
    });

    Ok(job)
}

/// Grab the frame at `time` as a full-resolution PNG, creating the folder if
/// it is not there yet. The destination is deterministic, so grabbing the same
/// moment twice replaces the file rather than making a second one.
pub fn screenshot(tools: &Tools, input: &Path, time: f64, dest: &Path) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }

    let out = ffmpeg::command(&tools.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-y", "-loglevel", "error"])
        .args(["-ss", &format!("{:.4}", time.max(0.0))])
        .arg("-i")
        .arg(input)
        .args(["-map", "0:v:0", "-frames:v", "1", "-update", "1"])
        .arg(dest)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;

    if out.status.success() && dest.is_file() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr)
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("ffmpeg produced no frame")
            .to_owned())
    }
}

/// Pull one frame out of the video as a PNG for the crop preview. ffmpeg
/// auto-rotates on decode, so the frame comes back display-oriented and lines
/// up with the rotated dimensions in `MediaInfo`.
pub fn poster_frame(
    tools: &Tools,
    input: &Path,
    duration: Option<f64>,
    max_width: u32,
) -> Result<image::RgbaImage, String> {
    let seek = duration
        .map(|d| (d * 0.1).min(d - 0.05).max(0.0))
        .unwrap_or(0.0);

    let dir = std::env::temp_dir().join("vid-sizer");
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make temp dir: {e}"))?;
    let out = dir.join(format!("poster_{}.png", std::process::id()));
    let _ = std::fs::remove_file(&out);

    let run = |args: &[String]| -> Result<(), String> {
        let res = ffmpeg::command(&tools.ffmpeg)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not run ffmpeg: {e}"))?;
        if res.status.success() && out.is_file() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&res.stderr)
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("ffmpeg produced no frame")
                .to_owned())
        }
    };

    let with_seek: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-ss".into(),
        format!("{seek:.3}"),
        "-i".into(),
        input.to_string_lossy().into_owned(),
        "-map".into(),
        "0:v:0".into(),
        "-frames:v".into(),
        "1".into(),
        "-vf".into(),
        format!("scale={max_width}:-2:flags=bicubic:force_original_aspect_ratio=decrease"),
        out.to_string_lossy().into_owned(),
    ];

    // Seeking can land past the end on files with a bogus duration, so retry
    // from the first frame before giving up.
    if run(&with_seek).is_err() {
        let from_start: Vec<String> = vec![
            "-hide_banner".into(),
            "-nostdin".into(),
            "-y".into(),
            "-i".into(),
            input.to_string_lossy().into_owned(),
            "-map".into(),
            "0:v:0".into(),
            "-frames:v".into(),
            "1".into(),
            "-vf".into(),
            format!("scale={max_width}:-2:flags=bicubic:force_original_aspect_ratio=decrease"),
            out.to_string_lossy().into_owned(),
        ];
        run(&from_start)?;
    }

    let img = image::open(&out)
        .map_err(|e| format!("could not read the extracted frame: {e}"))?
        .to_rgba8();
    let _ = std::fs::remove_file(&out);
    Ok(img)
}
