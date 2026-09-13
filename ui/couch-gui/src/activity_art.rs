//! Bounded, asynchronous artwork downloads. Auth stays on the configured Kodi host.
use crate::{home, App};
use image::{GenericImageView, ImageReader};
use std::{
    io::{Cursor, Read},
    sync::mpsc,
    time::Duration,
};
struct Job {
    generation: u64,
    key: String,
    host: String,
    connection: String,
    fanart: String,
    logo: String,
}
/// Decoded artwork, ready to become a Slint image on the UI thread.
#[derive(Clone)]
pub(crate) struct Pixels {
    width: u32,
    height: u32,
    data: Vec<u8>,
}
/// What an image is decoded into: the player backdrop with its legibility
/// gradient pre-composed, a small logo, or a thumbnail for a feedback card.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    Backdrop,
    Logo,
    Thumbnail,
}
struct Reply {
    generation: u64,
    key: String,
    fanart: Option<Pixels>,
    logo: Option<Pixels>,
}
pub struct Worker {
    tx: mpsc::SyncSender<Job>,
    rx: mpsc::Receiver<Reply>,
}
fn fetch(host: &str, connection: &str, path: &str, logo: bool) -> Option<Pixels> {
    if path.is_empty() {
        return None;
    }
    let settings: serde_json::Value = std::fs::read(home::path("kodi-web.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let scoped =
        couch_kodi::settings::Settings::load(&crate::connections::file(connection, "kodi"))
            .ok()
            .filter(|s| s.host == host);
    let scoped = scoped.and_then(|s| serde_json::to_value(s).ok());
    let settings = scoped.as_ref().unwrap_or(&settings[host]);
    let port = settings["web_port"]
        .as_u64()
        .or_else(|| settings["port"].as_u64())
        .and_then(|n| u16::try_from(n).ok())
        .unwrap_or(8080);
    let kodi = couch_kodi::Kodi::tcp(host, 9090).with_web_port(port);
    let url = kodi.image_url(path)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(4)))
        .max_redirects(0)
        .build()
        .into();
    let mut request = agent.get(&url);
    if let Some(user) = settings["username"].as_str() {
        use base64::Engine;
        let password = settings["password"].as_str().unwrap_or("");
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        request = request.header("Authorization", &format!("Basic {encoded}"));
    }
    let mut response = request.call().ok()?;
    let mut data = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut data)
        .ok()?;
    if data.len() > 8 * 1024 * 1024 {
        return None;
    }
    decode(&data, if logo { Shape::Logo } else { Shape::Backdrop })
}
pub(crate) fn decode(data: &[u8], shape: Shape) -> Option<Pixels> {
    let mut reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    if u64::from(image.width()) * u64::from(image.height()) > 20_000_000 {
        return None;
    }
    let image = match shape {
        Shape::Logo => image.thumbnail(384, 140),
        Shape::Thumbnail => image.resize_to_fill(112, 112, image::imageops::FilterType::Triangle),
        Shape::Backdrop => image.resize_to_fill(480, 800, image::imageops::FilterType::Triangle),
    };
    let (width, height) = image.dimensions();
    let mut rgba = image.to_rgba8();
    if shape == Shape::Backdrop {
        // Pre-compose the legibility gradient once, instead of blending a full
        // wallpaper every time the one-second progress label changes.
        for (_, y, pixel) in rgba.enumerate_pixels_mut() {
            let bottom = ((y as f32 - 300.) / 350.).clamp(0., 1.);
            let shade = (0.18 + bottom * 0.78).clamp(0., 0.96);
            for (channel, bg) in pixel.0[..3].iter_mut().zip([17., 19., 17.]) {
                *channel = (*channel as f32 * (1. - shade) + bg * shade) as u8;
            }
            pixel.0[3] = 255;
        }
    }
    Some(Pixels {
        width,
        height,
        data: rgba.into_raw(),
    })
}
pub(crate) fn slint_image(p: Pixels) -> slint::Image {
    slint::Image::from_rgba8(
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&p.data, p.width, p.height),
    )
}
impl Worker {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel::<Job>(1);
        let (reply, receive) = mpsc::sync_channel(2);
        std::thread::spawn(move || {
            while let Ok(job) = rx.recv() {
                // Decoded artwork is retained only by the bounded UI presentation
                // cache. A second worker cache previously ignored TTL/config edits.
                let fanart = fetch(&job.host, &job.connection, &job.fanart, false);
                let logo = fetch(&job.host, &job.connection, &job.logo, true);
                let result = Reply {
                    generation: job.generation,
                    key: job.key,
                    fanart,
                    logo,
                };
                if reply.send(result).is_err() {
                    return;
                }
            }
        });
        Self { tx, rx: receive }
    }
    pub fn request(
        &self,
        generation: u64,
        key: String,
        host: String,
        connection: String,
        fanart: String,
        logo: String,
    ) -> bool {
        self.tx
            .try_send(Job {
                generation,
                key,
                host,
                connection,
                fanart,
                logo,
            })
            .is_ok()
    }
    pub fn poll(&self, app: &App, generation: u64, key: &str) {
        while let Ok(reply) = self.rx.try_recv() {
            if reply.generation != generation || reply.key != key || !app.get_player_shown() {
                continue;
            }
            if let Some(p) = reply.fanart {
                app.set_player_fanart(slint_image(p));
                app.set_player_has_art(true)
            }
            if let Some(p) = reply.logo {
                app.set_player_logo(slint_image(p));
                app.set_player_has_logo(true)
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_art_is_a_fallback() {
        assert!(decode(b"not an image", Shape::Backdrop).is_none());
        assert!(decode(b"not an image", Shape::Thumbnail).is_none());
    }
}
