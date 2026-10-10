//! Video playback with synchronised audio.
//!
//! Two ffmpeg processes feed the player from the same seek point: one pipes
//! raw RGBA frames, the other raw f32 PCM at the output device's rate. Audio
//! is the master clock — the count of sample frames handed to the sound card
//! *is* the playback position — and video frames are shown when their
//! timestamp comes due, with late ones dropped. Without an audio track the
//! clock falls back to wall time.
//!
//! Seeking tears both processes down and starts them again at the new time,
//! bumping a generation counter so frames still in flight are discarded.

use std::collections::VecDeque;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::ffmpeg::{self, Tools};
use crate::probe::MediaInfo;
use crate::sizing;

/// Longest edge of a decoded preview frame. Small enough that a 4K source
/// still plays smoothly, large enough to frame a crop against.
const PREVIEW_MAX_EDGE: u32 = 960;

/// How many decoded frames may wait in the channel. The decoder blocks once
/// it is full, which is what keeps memory flat on a long clip.
const FRAME_QUEUE: usize = 6;

/// Roughly a second of stereo audio at 48 kHz; the feeder tops it up.
const AUDIO_HIGH_WATER: usize = 96_000;

pub struct Frame {
    pub pts: f64,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Shared between the audio callback and the feeder thread.
struct AudioShared {
    buf: VecDeque<f32>,
    /// Sample frames handed to the device since the last seek.
    frames_out: u64,
    /// Largest callback seen, used to estimate how far the device output
    /// trails what we have queued.
    callback_frames: u64,
    /// The feeder has reached the end of the stream.
    finished: bool,
}

pub struct Player {
    tools: Tools,
    path: PathBuf,
    duration: f64,
    fps: f64,

    /// Decoded frame size, which is the source size scaled down to fit
    /// `PREVIEW_MAX_EDGE`.
    pub width: u32,
    pub height: u32,

    has_audio: bool,
    sample_rate: u32,
    channels: u16,

    generation: Arc<AtomicU64>,
    rx: Option<Receiver<Frame>>,
    pending: Option<Frame>,
    /// Which generation already has a frame on screen. Paused playback shows
    /// exactly one frame per generation; without this the picture creeps
    /// forward on every repaint and ends up ahead of the audio on resume.
    shown_generation: Option<u64>,
    children: Arc<Mutex<Vec<Child>>>,
    decoder_done: Arc<AtomicBool>,

    audio: Arc<Mutex<AudioShared>>,
    volume: Arc<Mutex<f32>>,
    _stream: Option<cpal::Stream>,

    playing: bool,
    ended: bool,
    /// Playback time that the current generation started from.
    seek_base: f64,
    /// Wall-clock anchor for the silent path.
    wall_start: Option<Instant>,
    wall_accum: f64,

    pub audio_note: Option<String>,
}

impl Player {
    pub fn open(tools: &Tools, info: &MediaInfo, ctx: &egui::Context) -> Result<Self, String> {
        let longest = info.width.max(info.height).max(1);
        let factor = (PREVIEW_MAX_EDGE as f64 / longest as f64).min(1.0);
        let width = sizing::even_round(info.width as f64 * factor);
        let height = sizing::even_round(info.height as f64 * factor);

        let audio = Arc::new(Mutex::new(AudioShared {
            buf: VecDeque::new(),
            frames_out: 0,
            callback_frames: 0,
            finished: false,
        }));
        let volume = Arc::new(Mutex::new(0.8_f32));

        // Build the output stream first: the device decides the rate and
        // channel count that ffmpeg must then resample into.
        let mut audio_note = None;
        let mut stream = None;
        let mut sample_rate = 48_000;
        let mut channels = 2_u16;
        let mut has_audio = info.has_audio;

        if has_audio {
            match build_stream(Arc::clone(&audio), Arc::clone(&volume)) {
                Ok((s, rate, ch)) => {
                    stream = Some(s);
                    sample_rate = rate;
                    channels = ch;
                }
                Err(e) => {
                    has_audio = false;
                    audio_note = Some(format!("no audio output: {e}"));
                }
            }
        }

        let mut player = Self {
            tools: tools.clone(),
            path: info.path.clone(),
            duration: info.duration.unwrap_or(0.0),
            fps: info.fps.filter(|f| *f > 0.1 && *f < 1000.0).unwrap_or(30.0),
            width,
            height,
            has_audio,
            sample_rate,
            channels,
            generation: Arc::new(AtomicU64::new(0)),
            rx: None,
            pending: None,
            shown_generation: None,
            children: Arc::new(Mutex::new(Vec::new())),
            decoder_done: Arc::new(AtomicBool::new(false)),
            audio,
            volume,
            _stream: stream,
            playing: false,
            ended: false,
            seek_base: 0.0,
            wall_start: None,
            wall_accum: 0.0,
            audio_note,
        };

        // Decode from the start so there is a frame on screen immediately,
        // but stay paused until asked to play.
        player.restart_at(0.0, ctx)?;
        Ok(player)
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn has_audio(&self) -> bool {
        self.has_audio
    }

    pub fn volume(&self) -> f32 {
        *self.volume.lock().unwrap()
    }

    pub fn set_volume(&self, v: f32) {
        *self.volume.lock().unwrap() = v.clamp(0.0, 1.0);
    }

    /// Current playback position in seconds.
    pub fn position(&self) -> f64 {
        let offset = if self.has_audio {
            let a = self.audio.lock().unwrap();
            // What we handed the device leads what is audible by about one
            // callback, so trim that off to keep the picture honest.
            let lead = a.callback_frames as f64 / self.sample_rate as f64;
            (a.frames_out as f64 / self.sample_rate as f64 - lead).max(0.0)
        } else {
            self.wall_accum
                + self
                    .wall_start
                    .map(|t| t.elapsed().as_secs_f64())
                    .unwrap_or(0.0)
        };

        let pos = self.seek_base + offset;
        if self.duration > 0.0 {
            pos.clamp(0.0, self.duration)
        } else {
            pos.max(0.0)
        }
    }

    pub fn play(&mut self, ctx: &egui::Context) {
        if self.playing {
            return;
        }
        // Pressing play at the end starts over rather than doing nothing.
        if self.ended || (self.duration > 0.0 && self.position() >= self.duration - 0.05) {
            let _ = self.seek(0.0, ctx);
        }
        self.playing = true;
        self.ended = false;
        self.wall_start = Some(Instant::now());
        if let Some(s) = &self._stream {
            let _ = s.play();
        }
        ctx.request_repaint();
    }

    pub fn pause(&mut self) {
        if !self.playing {
            return;
        }
        self.playing = false;
        if let Some(t) = self.wall_start.take() {
            self.wall_accum += t.elapsed().as_secs_f64();
        }
        if let Some(s) = &self._stream {
            let _ = s.pause();
        }
    }

    pub fn toggle(&mut self, ctx: &egui::Context) {
        if self.playing {
            self.pause();
        } else {
            self.play(ctx);
        }
    }

    /// Jump to `time`, keeping the play/pause state.
    pub fn seek(&mut self, time: f64, ctx: &egui::Context) {
        let was_playing = self.playing;
        let target = if self.duration > 0.0 {
            time.clamp(0.0, (self.duration - 0.05).max(0.0))
        } else {
            time.max(0.0)
        };

        if let Some(s) = &self._stream {
            let _ = s.pause();
        }
        self.playing = false;

        if self.restart_at(target, ctx).is_ok() {
            self.ended = false;
            if was_playing {
                self.play(ctx);
            }
        }
    }

    /// Step one frame forward or back while paused.
    pub fn step(&mut self, frames: i32, ctx: &egui::Context) {
        self.pause();
        let delta = frames as f64 / self.fps;
        let t = (self.position() + delta).max(0.0);
        self.seek(t, ctx);
    }

    /// Tear down the current decode and start again from `time`.
    fn restart_at(&mut self, time: f64, ctx: &egui::Context) -> Result<(), String> {
        self.kill_children();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;

        self.pending = None;
        self.shown_generation = None;
        self.rx = None;
        self.decoder_done.store(false, Ordering::SeqCst);

        self.seek_base = time;
        self.wall_accum = 0.0;
        self.wall_start = None;

        {
            let mut a = self.audio.lock().unwrap();
            a.buf.clear();
            a.frames_out = 0;
            a.finished = false;
        }

        self.spawn_video(time, generation, ctx)?;
        if self.has_audio {
            // Audio is a nicety; a failure here should not stop the picture.
            if let Err(e) = self.spawn_audio(time, generation) {
                self.audio_note = Some(e);
                self.has_audio = false;
            }
        }
        Ok(())
    }

    fn spawn_video(
        &mut self,
        time: f64,
        generation: u64,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let filter = format!(
            "scale={}:{}:flags=bilinear,fps={},format=rgba",
            self.width, self.height, self.fps
        );
        let mut child = ffmpeg::command(&self.tools.ffmpeg)
            .args(["-hide_banner", "-nostdin", "-loglevel", "error"])
            .args(["-ss", &format!("{time:.4}")])
            .arg("-i")
            .arg(&self.path)
            .args(["-map", "0:v:0", "-an", "-vf", &filter])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start the video decoder: {e}"))?;

        let mut stdout = child.stdout.take().ok_or("no decoder stdout")?;
        self.children.lock().unwrap().push(child);

        let (tx, rx) = sync_channel::<Frame>(FRAME_QUEUE);
        self.rx = Some(rx);

        let (w, h, fps) = (self.width, self.height, self.fps);
        let gen_now = Arc::clone(&self.generation);
        let done = Arc::clone(&self.decoder_done);
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let frame_bytes = (w as usize) * (h as usize) * 4;
            let mut index: u64 = 0;
            loop {
                if gen_now.load(Ordering::SeqCst) != generation {
                    return; // a seek overtook us
                }
                let mut rgba = vec![0u8; frame_bytes];
                if stdout.read_exact(&mut rgba).is_err() {
                    break; // end of stream, or the process was killed
                }
                let frame = Frame {
                    pts: time + index as f64 / fps,
                    generation,
                    width: w,
                    height: h,
                    rgba,
                };
                index += 1;
                if tx.send(frame).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
            if gen_now.load(Ordering::SeqCst) == generation {
                done.store(true, Ordering::SeqCst);
                ctx.request_repaint();
            }
        });

        Ok(())
    }

    fn spawn_audio(&mut self, time: f64, generation: u64) -> Result<(), String> {
        let mut child = ffmpeg::command(&self.tools.ffmpeg)
            .args(["-hide_banner", "-nostdin", "-loglevel", "error"])
            .args(["-ss", &format!("{time:.4}")])
            .arg("-i")
            .arg(&self.path)
            .args(["-map", "0:a:0", "-vn"])
            .args(["-f", "f32le", "-acodec", "pcm_f32le"])
            .args(["-ac", &self.channels.to_string()])
            .args(["-ar", &self.sample_rate.to_string()])
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start the audio decoder: {e}"))?;

        let mut stdout = child.stdout.take().ok_or("no audio stdout")?;
        self.children.lock().unwrap().push(child);

        let shared = Arc::clone(&self.audio);
        let gen_now = Arc::clone(&self.generation);

        std::thread::spawn(move || {
            let mut raw = [0u8; 16_384];
            loop {
                if gen_now.load(Ordering::SeqCst) != generation {
                    return;
                }
                // Wait for the callback to drain before reading more, so a
                // long clip never buffers itself into memory.
                loop {
                    if gen_now.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    if shared.lock().unwrap().buf.len() < AUDIO_HIGH_WATER {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }

                match stdout.read(&mut raw) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let usable = n - (n % 4);
                        let mut s = shared.lock().unwrap();
                        if gen_now.load(Ordering::SeqCst) != generation {
                            return;
                        }
                        for chunk in raw[..usable].chunks_exact(4) {
                            s.buf.push_back(f32::from_le_bytes([
                                chunk[0], chunk[1], chunk[2], chunk[3],
                            ]));
                        }
                    }
                }
            }
            if gen_now.load(Ordering::SeqCst) == generation {
                shared.lock().unwrap().finished = true;
            }
        });

        Ok(())
    }

    /// Called once per UI frame. Returns the frame that is now due, if any.
    pub fn next_frame(&mut self, ctx: &egui::Context) -> Option<Frame> {
        let generation = self.generation.load(Ordering::SeqCst);
        let clock = self.position();
        let mut due: Option<Frame> = None;
        let mut disconnected = false;

        loop {
            if self.pending.is_none() {
                match self.rx.as_ref().map(|rx| rx.try_recv()) {
                    Some(Ok(f)) => self.pending = Some(f),
                    Some(Err(TryRecvError::Disconnected)) => {
                        disconnected = true;
                        break;
                    }
                    _ => break,
                }
            }

            let Some(f) = &self.pending else { break };
            if f.generation != generation {
                self.pending = None; // stale, from before a seek
                continue;
            }
            // Playing: take everything that has come due and keep the newest.
            // Paused: take exactly one frame per generation, so the picture
            // holds still instead of draining the decoder at repaint speed.
            let take = if self.playing {
                f.pts <= clock + 1e-6
            } else {
                due.is_none() && self.shown_generation != Some(generation)
            };
            if take {
                due = self.pending.take();
            } else {
                break;
            }
        }

        if due.is_some() {
            self.shown_generation = Some(generation);
        }

        if self.playing {
            if disconnected && self.decoder_done.load(Ordering::SeqCst) && self.pending.is_none() {
                self.ended = true;
                self.pause();
            }
            // Audio can run dry at the tail even when frames remain.
            if self.has_audio {
                let a = self.audio.lock().unwrap();
                if a.finished && a.buf.is_empty() && disconnected {
                    drop(a);
                    self.ended = true;
                    self.pause();
                }
            }
            ctx.request_repaint();
        }

        due
    }

    fn kill_children(&mut self) {
        for mut c in self.children.lock().unwrap().drain(..) {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Bump the generation first so the decode threads bail out, then stop
        // the processes they are reading from.
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(s) = &self._stream {
            let _ = s.pause();
        }
        self.kill_children();
    }
}

/// Open the default output device and start a stream that drains the shared
/// buffer. Returns the stream plus the rate and channel count ffmpeg must
/// match.
fn build_stream(
    shared: Arc<Mutex<AudioShared>>,
    volume: Arc<Mutex<f32>>,
) -> Result<(cpal::Stream, u32, u16), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("no default output device")?;
    let supported = device
        .default_output_config()
        .map_err(|e| format!("no default output config: {e}"))?;

    let sample_rate = supported.sample_rate();
    let channels = supported.channels();
    let config: cpal::StreamConfig = supported.config();
    let format = supported.sample_format();

    let err_fn = |e| eprintln!("audio stream error: {e}");

    macro_rules! build {
        ($t:ty, $convert:expr) => {{
            let shared = Arc::clone(&shared);
            let volume = Arc::clone(&volume);
            device.build_output_stream(
                config,
                move |out: &mut [$t], _: &cpal::OutputCallbackInfo| {
                    let gain = *volume.lock().unwrap();
                    let mut s = shared.lock().unwrap();
                    s.callback_frames = s
                        .callback_frames
                        .max((out.len() / channels.max(1) as usize) as u64);

                    let mut filled = 0usize;
                    for slot in out.iter_mut() {
                        match s.buf.pop_front() {
                            Some(v) => {
                                *slot = $convert(v * gain);
                                filled += 1;
                            }
                            // Underrun: silence beats a click.
                            None => *slot = $convert(0.0_f32),
                        }
                    }
                    s.frames_out += (filled / channels.max(1) as usize) as u64;
                },
                err_fn,
                None,
            )
        }};
    }

    let stream = match format {
        cpal::SampleFormat::F32 => build!(f32, |v: f32| v),
        cpal::SampleFormat::I16 => {
            build!(i16, |v: f32| (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        }
        cpal::SampleFormat::U16 => build!(u16, |v: f32| {
            ((v.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32) as u16
        }),
        other => return Err(format!("unsupported sample format {other:?}")),
    }
    .map_err(|e| format!("could not open the output stream: {e}"))?;

    // Built streams start running on some hosts; keep it quiet until play.
    let _ = stream.pause();

    Ok((stream, sample_rate, channels))
}

/// `1:02.5` style timecode for the transport bar.
pub fn timecode(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "0:00.0".to_owned();
    }
    let total = secs.floor() as u64;
    let tenths = ((secs - total as f64) * 10.0).floor() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{tenths}")
    } else {
        format!("{m}:{s:02}.{tenths}")
    }
}
