//! The logo: one PNG, used in the header band and as the window icon.

use eframe::egui;
use egui::Color32;

use crate::ui::HEADER_BG;

/// Swap this one file to change the logo — nothing else refers to it.
const LOGO_PNG: &[u8] = include_bytes!("../assets/logo.png");

pub struct LogoArt {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// The artwork's own background, read from a corner pixel, so the header
    /// band can be painted the same colour and the logo sits flush in it.
    pub background: Color32,
}

/// Decode the logo once and keep it. A bad replacement file falls back to a
/// single transparent pixel rather than stopping the app from starting.
pub fn logo_art() -> &'static LogoArt {
    use std::sync::OnceLock;
    static ART: OnceLock<LogoArt> = OnceLock::new();

    ART.get_or_init(|| match image::load_from_memory(LOGO_PNG) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (width, height) = (rgba.width(), rgba.height());
            let corner = rgba.get_pixel(0, 0).0;
            // A transparent corner means the art is cut out, so keep the
            // app's own header colour instead of fading to nothing.
            let background = if corner[3] < 16 {
                HEADER_BG
            } else {
                Color32::from_rgb(corner[0], corner[1], corner[2])
            };
            LogoArt {
                rgba: rgba.into_raw(),
                width,
                height,
                background,
            }
        }
        Err(_) => LogoArt {
            rgba: vec![0, 0, 0, 0],
            width: 1,
            height: 1,
            background: HEADER_BG,
        },
    })
}

/// A square version of the mark for the window and taskbar icon.
pub fn window_icon() -> egui::IconData {
    const SIDE: u32 = 256;
    let art = logo_art();
    let mut canvas = image::RgbaImage::from_pixel(SIDE, SIDE, image::Rgba([0, 0, 0, 0]));

    if let Some(src) = image::RgbaImage::from_raw(art.width, art.height, art.rgba.clone()) {
        // A wide wordmark padded into a square shrinks to an illegible smear
        // at 16px, so take the square mark off its leading edge instead.
        // Anything roughly square is padded as before.
        let wide = art.width as f32 / art.height.max(1) as f32 > 1.25;
        let source = if wide {
            let side = art.height.min(art.width);
            image::imageops::crop_imm(&src, 0, 0, side, side).to_image()
        } else {
            src
        };

        let (w, h) = (source.width(), source.height());
        let fit = if wide {
            SIDE as f32 / w.max(h) as f32
        } else {
            // Taskbar icons are cropped tight, so cut-out art gets a margin.
            (SIDE as f32 * 0.92) / w.max(h) as f32
        };
        let (dw, dh) = (
            ((w as f32 * fit) as u32).clamp(1, SIDE),
            ((h as f32 * fit) as u32).clamp(1, SIDE),
        );
        let scaled =
            image::imageops::resize(&source, dw, dh, image::imageops::FilterType::CatmullRom);
        let (ox, oy) = (((SIDE - dw) / 2) as i64, ((SIDE - dh) / 2) as i64);
        image::imageops::overlay(&mut canvas, &scaled, ox, oy);
    }

    egui::IconData {
        rgba: canvas.into_raw(),
        width: SIDE,
        height: SIDE,
    }
}
