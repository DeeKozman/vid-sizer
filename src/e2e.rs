//! End-to-end checks that actually shell out to ffmpeg.
//!
//! These need real media, so they are `#[ignore]`d and only run on demand:
//!
//! ```text
//! set VIDSIZER_FIXTURES=C:\path\to\folder\of\videos
//! cargo test -- --ignored --nocapture
//! ```
//!
//! The folder is expected to hold `wide_1920x1080.mp4`, `tall_1080x1920.mp4`
//! and `four_three_640x480.mp4`, which `make_fixtures.ps1` generates.

use std::path::PathBuf;
use std::sync::mpsc::channel;

use crate::ffmpeg::Tools;
use crate::job::{self, EncodeSpec, JobMsg};
use crate::{probe, sizing};

fn fixtures() -> PathBuf {
    PathBuf::from(
        std::env::var("VIDSIZER_FIXTURES")
            .expect("set VIDSIZER_FIXTURES to a folder holding the test clips"),
    )
}

/// Run an encode to completion on a throwaway egui context.
fn encode(tools: &Tools, spec: EncodeSpec) -> Result<PathBuf, String> {
    let (tx, rx) = channel();
    let ctx = egui::Context::default();
    let _job = job::spawn(tools, spec, tx, ctx)?;
    loop {
        match rx.recv() {
            Ok(JobMsg::Progress { .. }) => {}
            Ok(JobMsg::Done(p)) => return Ok(p),
            Ok(JobMsg::Failed(e)) => return Err(e),
            Ok(JobMsg::Cancelled) => return Err("cancelled".into()),
            Err(e) => return Err(format!("channel closed: {e}")),
        }
    }
}

/// Read back the real dimensions of a file we just wrote.
fn dims(tools: &Tools, p: &PathBuf) -> (u32, u32) {
    let info = probe::probe(tools, p).expect("probe output");
    (info.width, info.height)
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn probe_reads_a_real_file() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let info = probe::probe(&tools, &fixtures().join("wide_1920x1080.mp4")).unwrap();

    assert_eq!((info.width, info.height), (1920, 1080));
    assert!(info.has_audio, "fixture should carry an audio track");
    assert!(info.fps.unwrap() > 29.0);
    assert!(info.duration.unwrap() > 2.5);
    assert_eq!(info.rotation, 0);

    let silent = probe::probe(&tools, &fixtures().join("tall_1080x1920.mp4")).unwrap();
    assert_eq!((silent.width, silent.height), (1080, 1920));
    assert!(!silent.has_audio);
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn poster_frame_comes_back_display_oriented() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let src = fixtures().join("tall_1080x1920.mp4");
    let img = job::poster_frame(&tools, &src, Some(2.0), 1280).unwrap();
    // A 9:16 source must come back taller than it is wide.
    assert!(
        img.height() > img.width(),
        "{}x{}",
        img.width(),
        img.height()
    );
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn percent_resize_writes_the_named_file() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let src = fixtures().join("wide_1920x1080.mp4");
    let info = probe::probe(&tools, &src).unwrap();

    let (w, h) = (
        sizing::even_round(info.width as f64 * 0.5),
        sizing::even_round(info.height as f64 * 0.5),
    );
    let out = sizing::output_path(&src, w, h);

    let spec = EncodeSpec {
        input: src.clone(),
        output: out.clone(),
        crop: None,
        width: w,
        height: h,
        crf: 28,
        preset: "ultrafast".into(),
        duration: info.duration,
        copy_audio: true,
        trim: None,
        stream_copy: false,
        mp3_kbps: None,
    };
    let written = encode(&tools, spec).unwrap();

    assert!(written.exists());
    assert_eq!(
        written.file_name().unwrap().to_string_lossy(),
        "wide_1920x1080_960x540.mp4",
        "output must be named after the new pixel dimensions"
    );
    assert_eq!(dims(&tools, &written), (960, 540));
    assert_eq!(
        written.parent(),
        src.parent(),
        "output belongs next to the source"
    );
    std::fs::remove_file(&written).ok();
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn mismatched_aspect_crops_to_the_exact_target() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let src = fixtures().join("wide_1920x1080.mp4");
    let info = probe::probe(&tools, &src).unwrap();

    // 16:9 into a 9:16 vertical frame: the widest possible mismatch.
    let (tw, th) = (1080, 1920);
    assert!(sizing::needs_crop(info.width, info.height, tw, th));
    let (cw, ch) = sizing::max_crop(info.width, info.height, tw, th);
    assert_eq!((cw, ch), (606, 1080));

    // Pan fully to the right edge and confirm the offset stays in bounds.
    let x = sizing::even_down((info.width - cw) as f64);
    assert_eq!(x, 1314);
    assert!(x + cw <= info.width);

    let out = sizing::output_path(&src, tw, th);
    let spec = EncodeSpec {
        input: src.clone(),
        output: out,
        crop: Some(job::Crop {
            w: cw,
            h: ch,
            x,
            y: 0,
        }),
        width: tw,
        height: th,
        crf: 30,
        preset: "ultrafast".into(),
        duration: info.duration,
        copy_audio: true,
        trim: None,
        stream_copy: false,
        mp3_kbps: None,
    };
    let written = encode(&tools, spec).unwrap();

    assert_eq!(
        written.file_name().unwrap().to_string_lossy(),
        "wide_1920x1080_1080x1920.mp4"
    );
    assert_eq!(dims(&tools, &written), (1080, 1920));
    std::fs::remove_file(&written).ok();
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn silent_source_encodes_without_an_audio_map() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let src = fixtures().join("tall_1080x1920.mp4");
    let info = probe::probe(&tools, &src).unwrap();

    let out = sizing::output_path(&src, 540, 960);
    let spec = EncodeSpec {
        input: src,
        output: out,
        crop: None,
        width: 540,
        height: 960,
        crf: 30,
        preset: "ultrafast".into(),
        duration: info.duration,
        // The UI ands this with has_audio; mirror that here.
        copy_audio: false,
        trim: None,
        stream_copy: false,
        mp3_kbps: None,
    };
    let written = encode(&tools, spec).unwrap();
    assert_eq!(dims(&tools, &written), (540, 960));
    std::fs::remove_file(&written).ok();
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn four_three_to_1080p_crops_top_and_bottom() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let src = fixtures().join("four_three_640x480.mp4");
    let info = probe::probe(&tools, &src).unwrap();

    let (tw, th) = (1920, 1080);
    let (cw, ch) = sizing::max_crop(info.width, info.height, tw, th);
    assert_eq!((cw, ch), (640, 360));

    let out = sizing::output_path(&src, tw, th);
    let spec = EncodeSpec {
        input: src,
        output: out,
        crop: Some(job::Crop {
            w: cw,
            h: ch,
            x: 0,
            y: sizing::even_down((info.height - ch) as f64 / 2.0),
        }),
        width: tw,
        height: th,
        crf: 30,
        preset: "ultrafast".into(),
        duration: info.duration,
        copy_audio: false,
        trim: None,
        stream_copy: false,
        mp3_kbps: None,
    };
    let written = encode(&tools, spec).unwrap();
    assert_eq!(dims(&tools, &written), (1920, 1080));
    std::fs::remove_file(&written).ok();
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn name_collisions_get_a_suffix() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let src = fixtures().join("four_three_640x480.mp4");
    let info = probe::probe(&tools, &src).unwrap();

    let mut written = Vec::new();
    for _ in 0..2 {
        let out = sizing::output_path(&src, 320, 240);
        let spec = EncodeSpec {
            input: src.clone(),
            output: out,
            crop: None,
            width: 320,
            height: 240,
            crf: 32,
            preset: "ultrafast".into(),
            duration: info.duration,
            copy_audio: false,
            trim: None,
            stream_copy: false,
            mp3_kbps: None,
        };
        written.push(encode(&tools, spec).unwrap());
    }

    assert_eq!(
        written[0].file_name().unwrap().to_string_lossy(),
        "four_three_640x480_320x240.mp4"
    );
    assert_eq!(
        written[1].file_name().unwrap().to_string_lossy(),
        "four_three_640x480_320x240_2.mp4",
        "a second render must not clobber the first"
    );
    for p in written {
        std::fs::remove_file(p).ok();
    }
}

// ------------------------------------------------------------- playback

use crate::player::{self, Player};

/// Spin the player until a frame arrives or the deadline passes.
fn wait_for_frame(p: &mut Player, ctx: &egui::Context, secs: f64) -> Option<crate::player::Frame> {
    let start = std::time::Instant::now();
    loop {
        if let Some(f) = p.next_frame(ctx) {
            return Some(f);
        }
        if start.elapsed().as_secs_f64() > secs {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn timecode_formats_minutes_and_hours() {
    assert_eq!(player::timecode(0.0), "0:00.0");
    assert_eq!(player::timecode(62.5), "1:02.5");
    assert_eq!(player::timecode(3661.0), "1:01:01.0");
    assert_eq!(player::timecode(f64::NAN), "0:00.0");
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn player_decodes_preview_frames_at_the_scaled_size() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let info = probe::probe(&tools, &fixtures().join("tall_1080x1920.mp4")).unwrap();
    let ctx = egui::Context::default();

    let mut p = Player::open(&tools, &info, &ctx).expect("player opens");
    // 1080x1920 scaled so the longest edge is 960.
    assert_eq!((p.width, p.height), (540, 960));
    assert!(!p.is_playing(), "should open paused");

    let frame = wait_for_frame(&mut p, &ctx, 10.0).expect("a frame arrives");
    assert_eq!((frame.width, frame.height), (540, 960));
    assert_eq!(frame.rgba.len(), 540 * 960 * 4);
    assert!(p.position() < 0.1, "starts at the beginning");
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn playing_advances_the_clock_and_delivers_frames() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let info = probe::probe(&tools, &fixtures().join("wide_1920x1080.mp4")).unwrap();
    let ctx = egui::Context::default();

    let mut p = Player::open(&tools, &info, &ctx).expect("player opens");
    wait_for_frame(&mut p, &ctx, 10.0).expect("first frame");

    p.play(&ctx);
    assert!(p.is_playing());

    let mut frames = 0;
    let start = std::time::Instant::now();
    while start.elapsed().as_secs_f64() < 1.2 {
        if p.next_frame(&ctx).is_some() {
            frames += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    let pos = p.position();
    assert!(frames > 5, "expected a stream of frames, got {frames}");
    assert!(pos > 0.3, "clock should have advanced, sat at {pos}");
    assert!(pos < 2.5, "clock ran away to {pos}");

    p.pause();
    assert!(!p.is_playing());
    let held = p.position();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        (p.position() - held).abs() < 0.02,
        "clock must freeze while paused"
    );
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn seeking_moves_the_clock_and_discards_stale_frames() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let info = probe::probe(&tools, &fixtures().join("wide_1920x1080.mp4")).unwrap();
    let ctx = egui::Context::default();

    let mut p = Player::open(&tools, &info, &ctx).expect("player opens");
    wait_for_frame(&mut p, &ctx, 10.0).expect("first frame");

    p.seek(2.0, &ctx);
    assert!(
        (p.position() - 2.0).abs() < 0.15,
        "position after seek was {}",
        p.position()
    );
    // A fresh frame must still arrive from the new point.
    let frame = wait_for_frame(&mut p, &ctx, 10.0).expect("frame after seek");
    assert!(
        frame.pts >= 1.9,
        "frame pts {} is from before the seek",
        frame.pts
    );

    // Seeking past the end is clamped rather than hanging.
    p.seek(999.0, &ctx);
    assert!(p.position() <= info.duration.unwrap() + 0.01);
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn stepping_a_frame_moves_by_one_frame_duration() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let info = probe::probe(&tools, &fixtures().join("wide_1920x1080.mp4")).unwrap();
    let ctx = egui::Context::default();

    let mut p = Player::open(&tools, &info, &ctx).expect("player opens");
    wait_for_frame(&mut p, &ctx, 10.0).expect("first frame");

    p.seek(1.0, &ctx);
    let before = p.position();
    p.step(1, &ctx);
    let delta = p.position() - before;
    let one_frame = 1.0 / info.fps.unwrap();
    assert!(
        (delta - one_frame).abs() < one_frame * 0.5,
        "stepped {delta}s, expected about {one_frame}s"
    );
    assert!(!p.is_playing(), "stepping should leave it paused");
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn volume_is_clamped_and_silent_sources_report_no_audio() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let ctx = egui::Context::default();

    let silent = probe::probe(&tools, &fixtures().join("tall_1080x1920.mp4")).unwrap();
    let p = Player::open(&tools, &silent, &ctx).expect("player opens");
    assert!(!p.has_audio(), "fixture has no audio track");

    let noisy = probe::probe(&tools, &fixtures().join("wide_1920x1080.mp4")).unwrap();
    let p = Player::open(&tools, &noisy, &ctx).expect("player opens");
    p.set_volume(5.0);
    assert_eq!(p.volume(), 1.0);
    p.set_volume(-1.0);
    assert_eq!(p.volume(), 0.0);
}

// --------------------------------------------- screenshots and quick export

#[test]
fn trim_args_seek_before_the_input() {
    // -ss has to come before -i or ffmpeg decodes the whole file to get there.
    let spec = EncodeSpec {
        input: PathBuf::from("in.mp4"),
        output: PathBuf::from("out.mp4"),
        crop: None,
        width: 640,
        height: 360,
        crf: 18,
        preset: "medium".into(),
        duration: Some(30.0),
        copy_audio: true,
        trim: Some((5.0, 12.5)),
        stream_copy: false,
        mp3_kbps: None,
    };
    let args = spec.args();
    let ss = args.iter().position(|a| a == "-ss").expect("-ss present");
    let i = args.iter().position(|a| a == "-i").expect("-i present");
    assert!(ss < i, "-ss must precede -i: {args:?}");
    assert_eq!(args[ss + 1], "5.000");

    // Length is given as a duration, because an input seek rebases to zero.
    let t = args.iter().position(|a| a == "-t").expect("-t present");
    assert!(t > i, "-t belongs after -i");
    assert_eq!(args[t + 1], "7.500");
    assert_eq!(spec.work_seconds(), Some(7.5), "progress tracks the trim");

    // Untrimmed jobs carry neither flag.
    let whole = EncodeSpec {
        trim: None,
        ..spec.clone()
    };
    assert!(!whole.args().contains(&"-t".to_string()));
    assert_eq!(whole.work_seconds(), Some(30.0));
}

#[test]
fn stream_copy_skips_the_filter_chain() {
    let spec = EncodeSpec {
        input: PathBuf::from("in.mp4"),
        output: PathBuf::from("out.mp4"),
        crop: Some(job::Crop {
            w: 100,
            h: 100,
            x: 0,
            y: 0,
        }),
        width: 640,
        height: 360,
        crf: 18,
        preset: "medium".into(),
        duration: Some(30.0),
        copy_audio: true,
        trim: Some((1.0, 2.0)),
        stream_copy: true,
        mp3_kbps: None,
    };
    let args = spec.args();
    assert!(
        !args.contains(&"-vf".to_string()),
        "no filtering on a remux"
    );
    assert!(!args.contains(&"libx264".to_string()));
    assert!(args.windows(2).any(|w| w == ["-c", "copy"]));
    assert!(args.contains(&"-avoid_negative_ts".to_string()));
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn quick_export_writes_only_the_marked_range() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let src = fixtures().join("wide_1920x1080.mp4");
    let info = probe::probe(&tools, &src).unwrap();

    let (start, end) = (0.5, 2.0);
    let out = sizing::trim_output_path(&src, 960, 540, start, end);
    let spec = EncodeSpec {
        input: src.clone(),
        output: out,
        crop: None,
        width: 960,
        height: 540,
        crf: 30,
        preset: "ultrafast".into(),
        duration: info.duration,
        copy_audio: true,
        trim: Some((start, end)),
        stream_copy: false,
        mp3_kbps: None,
    };
    let written = encode(&tools, spec).unwrap();

    assert_eq!(
        written.file_name().unwrap().to_string_lossy(),
        "wide_1920x1080_00-00-00-500-00-00-02-000_960x540.mp4"
    );
    let got = probe::probe(&tools, &written).unwrap();
    assert_eq!((got.width, got.height), (960, 540));
    let len = got.duration.unwrap();
    assert!(
        (len - (end - start)).abs() < 0.25,
        "expected about {}s, got {len}s",
        end - start
    );
    std::fs::remove_file(&written).ok();
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn fast_trim_keeps_the_source_untouched() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let src = fixtures().join("wide_1920x1080.mp4");
    let info = probe::probe(&tools, &src).unwrap();

    let (start, end) = (0.0, 1.5);
    let out = sizing::trim_output_path(&src, info.width, info.height, start, end);
    let spec = EncodeSpec {
        input: src.clone(),
        output: out,
        crop: None,
        width: info.width,
        height: info.height,
        crf: 18,
        preset: "medium".into(),
        duration: info.duration,
        copy_audio: true,
        trim: Some((start, end)),
        stream_copy: true,
        mp3_kbps: None,
    };
    let written = encode(&tools, spec).unwrap();

    let got = probe::probe(&tools, &written).unwrap();
    assert_eq!(
        (got.width, got.height),
        (info.width, info.height),
        "a remux must not resize"
    );
    assert!(got.has_audio, "audio should survive the copy");
    std::fs::remove_file(&written).ok();
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn screenshots_are_full_resolution_and_deterministic() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let src = fixtures().join("wide_1920x1080.mp4");

    let at = 1.25;
    let dest = sizing::shot_path(&src, at);
    assert_eq!(
        dest.file_name().unwrap().to_string_lossy(),
        "wide_1920x1080_00-00-01-250.png"
    );
    assert_eq!(dest.parent().unwrap(), sizing::shot_folder(&src));

    job::screenshot(&tools, &src, at, &dest).expect("screenshot written");
    assert!(dest.is_file(), "the folder should have been created");

    let img = image::open(&dest).unwrap();
    assert_eq!(
        (img.width(), img.height()),
        (1920, 1080),
        "grabs are full resolution, not the preview size"
    );

    // Grabbing the same moment twice lands on the same file, not a second one.
    let again = sizing::shot_path(&src, at);
    assert_eq!(again, dest);
    job::screenshot(&tools, &src, at, &again).expect("second screenshot");
    let count = std::fs::read_dir(sizing::shot_folder(&src))
        .unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert_eq!(count, 1, "a repeat grab must replace, not pile up");

    std::fs::remove_dir_all(sizing::shot_folder(&src)).ok();
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn paused_playback_holds_a_single_frame() {
    // Regression: paused used to take one frame per repaint, so the picture
    // crept forward and the video ended up ahead of the audio on resume.
    let tools = Tools::locate().expect("ffmpeg tools");
    let info = probe::probe(&tools, &fixtures().join("wide_1920x1080.mp4")).unwrap();
    let ctx = egui::Context::default();

    let mut p = Player::open(&tools, &info, &ctx).expect("player opens");
    let first = wait_for_frame(&mut p, &ctx, 10.0).expect("first frame");

    // Hammer it the way the UI would while sitting paused.
    let mut extra = 0;
    let start = std::time::Instant::now();
    while start.elapsed().as_secs_f64() < 0.8 {
        if p.next_frame(&ctx).is_some() {
            extra += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    assert_eq!(extra, 0, "paused player handed out {extra} extra frames");
    assert!(p.position() < 0.1, "clock moved while paused");

    // A seek is allowed to deliver exactly one new frame to show.
    p.seek(1.0, &ctx);
    let after = wait_for_frame(&mut p, &ctx, 10.0).expect("frame after seek");
    assert!(after.pts > first.pts);

    let mut extra = 0;
    let start = std::time::Instant::now();
    while start.elapsed().as_secs_f64() < 0.5 {
        if p.next_frame(&ctx).is_some() {
            extra += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    assert_eq!(
        extra, 0,
        "still paused, but handed out {extra} extra frames"
    );
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn resuming_picks_up_where_the_pause_left_off() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let info = probe::probe(&tools, &fixtures().join("wide_1920x1080.mp4")).unwrap();
    let ctx = egui::Context::default();

    let mut p = Player::open(&tools, &info, &ctx).expect("player opens");
    wait_for_frame(&mut p, &ctx, 10.0).expect("first frame");

    p.play(&ctx);
    let start = std::time::Instant::now();
    while start.elapsed().as_secs_f64() < 0.6 {
        p.next_frame(&ctx);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    p.pause();
    let at_pause = p.position();

    // Sit paused a while, as a person would.
    let start = std::time::Instant::now();
    while start.elapsed().as_secs_f64() < 0.9 {
        p.next_frame(&ctx);
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    assert!(
        (p.position() - at_pause).abs() < 0.03,
        "clock drifted from {at_pause} to {} while paused",
        p.position()
    );

    // On resume the picture must start moving again promptly, rather than
    // freezing while the audio clock catches up to buffered frames.
    p.play(&ctx);
    let mut frames = 0;
    let start = std::time::Instant::now();
    while start.elapsed().as_secs_f64() < 0.8 {
        if p.next_frame(&ctx).is_some() {
            frames += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(frames > 3, "only {frames} frames after resuming");
    assert!(p.position() > at_pause, "clock did not resume");
}

// ----------------------------------------------------------------- mp3 rips

#[test]
fn mp3_args_drop_the_video_and_the_filter_chain() {
    let spec = EncodeSpec {
        input: PathBuf::from("in.mp4"),
        output: PathBuf::from("out.mp3"),
        crop: Some(job::Crop {
            w: 10,
            h: 10,
            x: 0,
            y: 0,
        }),
        width: 640,
        height: 360,
        crf: 18,
        preset: "medium".into(),
        duration: Some(30.0),
        copy_audio: true,
        trim: None,
        stream_copy: false,
        mp3_kbps: Some(320),
    };
    let args = spec.args();
    assert!(args.contains(&"-vn".to_string()), "video must be dropped");
    assert!(!args.contains(&"-vf".to_string()), "no filtering on a rip");
    assert!(!args.contains(&"libx264".to_string()));
    assert!(args.windows(2).any(|w| w == ["-c:a", "libmp3lame"]));
    assert!(args.windows(2).any(|w| w == ["-b:a", "320k"]));

    // A marked range still seeks before the input.
    let ranged = EncodeSpec {
        trim: Some((2.0, 5.0)),
        ..spec.clone()
    };
    let args = ranged.args();
    let ss = args.iter().position(|a| a == "-ss").unwrap();
    let i = args.iter().position(|a| a == "-i").unwrap();
    assert!(ss < i);
    assert_eq!(ranged.work_seconds(), Some(3.0));
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn mp3_rip_writes_audio_only() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let src = fixtures().join("wide_1920x1080.mp4");
    let info = probe::probe(&tools, &src).unwrap();

    let out = sizing::mp3_output_path(&src, None);
    assert_eq!(out.file_name().unwrap(), "wide_1920x1080.mp3");

    let spec = EncodeSpec {
        input: src.clone(),
        output: out,
        crop: None,
        width: info.width,
        height: info.height,
        crf: 18,
        preset: "medium".into(),
        duration: info.duration,
        copy_audio: true,
        trim: None,
        stream_copy: false,
        mp3_kbps: Some(320),
    };
    let written = encode(&tools, spec).unwrap();
    assert!(written.is_file());

    // There should be no video stream to find.
    assert!(
        probe::probe(&tools, &written).is_err(),
        "an mp3 must not probe as video"
    );
    assert!(
        std::fs::metadata(&written).unwrap().len() > 1000,
        "the rip came out suspiciously small"
    );
    std::fs::remove_file(&written).ok();
}

#[test]
#[ignore = "needs ffmpeg and VIDSIZER_FIXTURES"]
fn mp3_rip_of_a_marked_range_is_that_long() {
    let tools = Tools::locate().expect("ffmpeg tools");
    let src = fixtures().join("wide_1920x1080.mp4");
    let info = probe::probe(&tools, &src).unwrap();

    let (start, end) = (0.5, 2.0);
    let out = sizing::mp3_output_path(&src, Some((start, end)));
    assert_eq!(
        out.file_name().unwrap(),
        "wide_1920x1080_00-00-00-500-00-00-02-000.mp3"
    );

    let spec = EncodeSpec {
        input: src,
        output: out,
        crop: None,
        width: info.width,
        height: info.height,
        crf: 18,
        preset: "medium".into(),
        duration: info.duration,
        copy_audio: true,
        trim: Some((start, end)),
        stream_copy: false,
        mp3_kbps: Some(320),
    };
    let written = encode(&tools, spec).unwrap();

    // Read the length back with ffprobe rather than trusting the request.
    let out = crate::ffmpeg::command(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(&written)
        .output()
        .unwrap();
    let len: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    assert!(
        (len - (end - start)).abs() < 0.2,
        "expected about {}s of audio, got {len}s",
        end - start
    );
    std::fs::remove_file(&written).ok();
}
