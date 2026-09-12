//! What a picture costs this model, measured rather than guessed.
//!
//! A Qwen-VL charges by area — a thousand pixels to a token on the machine
//! this was written on — while a Gemma charges a flat rate per image whatever
//! its size. No formula covers both, so the number comes from the server:
//! two throwaway requests, one with a picture and one without, and the
//! difference is the answer. It is kept per model, because it is a property
//! of the model.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What one model charges for an image.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ImageCost {
    /// Tokens per pixel; zero for a model that charges a flat rate.
    pub per_pixel: f32,
    /// Tokens charged for a picture before its size is counted.
    pub fixed: f32,
}

impl ImageCost {
    /// Tokens an image of this size will cost.
    pub fn tokens(&self, width: u32, height: u32) -> u32 {
        (self.fixed + self.per_pixel * width as f32 * height as f32).round().max(0.0) as u32
    }

    /// How that reads next to the attachment: `~1.2k tokens`.
    pub fn label(&self, width: u32, height: u32) -> String {
        let t = self.tokens(width, height);
        if t >= 1000 {
            format!("~{:.1}k tokens", t as f32 / 1000.0)
        } else {
            format!("~{t} tokens")
        }
    }
}

/// Measured costs, per model, kept between runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImageCosts {
    #[serde(default)]
    models: BTreeMap<String, ImageCost>,
}

impl ImageCosts {
    fn path() -> Option<PathBuf> {
        if let Ok(exe) = std::env::current_exe() {
            let exe = exe.to_string_lossy().to_string();
            if exe.contains("/deps/") || exe.contains("\\deps\\") {
                return Some(std::env::temp_dir().join("flashagent_test_image_costs.json"));
            }
        }
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()
            .map(|h| PathBuf::from(h).join(".flashagent").join("image-costs.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    /// What is known about this model, if anything.
    pub fn get(&self, model: &str) -> Option<ImageCost> {
        self.models.get(model).copied()
    }

    /// Record a measurement.
    pub fn set(&mut self, model: &str, cost: ImageCost) {
        self.models.insert(model.to_string(), cost);
    }
}

/// The picture used to measure with.
///
/// 448×448 rather than something smaller: the cost per pixel is not quite
/// constant, and extrapolating a screenshot from a thumbnail undershot by a
/// fifth when this was checked against a real 1280×720. A probe this size
/// costs a couple of hundred tokens, once per model, and lands within a few
/// percent at screenshot sizes.
pub const PROBE_WIDTH: u32 = 448;
pub const PROBE_HEIGHT: u32 = 448;

/// A 224×224 PNG of flat grey. Built rather than embedded so there is no
/// binary blob to explain.
pub fn probe_png() -> Vec<u8> {
    // One scanline: filter byte 0, then RGB triples.
    let mut row = vec![0u8]; // filter byte: none
    row.extend(std::iter::repeat_n(128u8, (PROBE_WIDTH * 3) as usize));
    let mut raw = Vec::with_capacity(row.len() * PROBE_HEIGHT as usize);
    for _ in 0..PROBE_HEIGHT {
        raw.extend_from_slice(&row);
    }
    png_from_rgb(&raw, PROBE_WIDTH, PROBE_HEIGHT)
}

/// Wrap raw filtered scanlines into a PNG.
fn png_from_rgb(raw: &[u8], width: u32, height: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for (i, entry) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *entry = c;
        }
        let mut c = 0xFFFF_FFFFu32;
        for b in bytes {
            c = table[((c ^ *b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        c ^ 0xFFFF_FFFF
    }

    // Stored (uncompressed) deflate blocks: no compressor needed, and the
    // probe is thrown away after one request.
    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01];
        for (i, block) in data.chunks(65_535).enumerate() {
            let last = (i + 1) * 65_535 >= data.len();
            out.push(if last { 1 } else { 0 });
            out.extend_from_slice(&(block.len() as u16).to_le_bytes());
            out.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
            out.extend_from_slice(block);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for byte in data {
            a = (a + *byte as u32) % 65_521;
            b = (b + a) % 65_521;
        }
        out.extend_from_slice(&((b << 16) | a).to_be_bytes());
        out
    }

    let chunk = |kind: &[u8], data: &[u8]| -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
        out
    };

    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);

    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(&chunk(b"IHDR", &ihdr));
    png.extend_from_slice(&chunk(b"IDAT", &zlib_stored(raw)));
    png.extend_from_slice(&chunk(b"IEND", b""));
    png
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_is_a_real_png_of_the_size_it_claims() {
        // It is sent to a server; a malformed file would measure nothing.
        let png = probe_png();
        assert_eq!(
            crate::clipboard::image_dimensions(&png),
            Some((PROBE_WIDTH, PROBE_HEIGHT))
        );
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        assert!(png.ends_with(b"IEND\xAE\x42\x60\x82"), "the end marker and its checksum");
    }

    #[test]
    fn a_model_charging_by_area_is_extrapolated_from_the_measurement() {
        // Measured on qwen3.6-35b: about one token per thousand pixels.
        let cost = ImageCost { per_pixel: 1.0 / 1000.0, fixed: 2.0 };
        assert_eq!(cost.tokens(1280, 720), 924);
        assert_eq!(cost.label(1280, 720), "~924 tokens");
        assert_eq!(cost.label(1920, 1080), "~2.1k tokens", "past a thousand it is rounded");
        assert_eq!(cost.tokens(260, 140), 38);
        assert_eq!(cost.label(260, 140), "~38 tokens");
    }

    #[test]
    fn a_model_charging_a_flat_rate_says_the_same_for_every_size() {
        // Gemma-style: one price per picture, whatever its size.
        let cost = ImageCost { per_pixel: 0.0, fixed: 256.0 };
        assert_eq!(cost.tokens(64, 64), 256);
        assert_eq!(cost.tokens(4000, 3000), 256);
    }

    #[test]
    fn what_was_measured_survives_a_restart() {
        let mut costs = ImageCosts::default();
        assert_eq!(costs.get("m"), None, "nothing is claimed before it is measured");
        costs.set("m", ImageCost { per_pixel: 0.001, fixed: 2.0 });
        let back: ImageCosts = serde_json::from_str(&serde_json::to_string(&costs).unwrap()).unwrap();
        assert_eq!(back.get("m").unwrap().tokens(1000, 1000), 1002);
    }
}
