//! Cross-platform clipboard support: OSC 52 terminal sequences, arboard,
//! and native OS tool fallbacks (wl-clipboard, xclip, pbcopy/pbpaste).

use std::io::Write as _;
use std::process::{Command, Stdio};

pub use flashagent_core::base64_encode;

/// Set system clipboard text using OSC 52, arboard, and platform CLI tools.
pub fn set_clipboard_text(text: &str) -> bool {
    // 1. OSC 52 sequence to stdout (works natively in tmux, Kitty, Alacritty, WezTerm, Windows Terminal)
    let b64 = base64_encode(text.as_bytes());
    let _ = write!(std::io::stdout(), "\x1b]52;c;{b64}\x07");
    let _ = std::io::stdout().flush();
    let success = true;

    // 2. Arboard system clipboard
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if cb.set_text(text.to_string()).is_ok() {
            return true;
        }
    }

    // 3. Fallback CLI tools for Linux/Unix/macOS/Windows
    #[cfg(target_os = "linux")]
    {
        // Try wl-copy (Wayland)
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if let Ok(mut child) = Command::new("wl-copy").stdin(Stdio::piped()).spawn() {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                if child.wait().is_ok() {
                    return true;
                }
            }
        }

        // Try xclip (X11)
        if let Ok(mut child) = Command::new("xclip")
            .arg("-selection")
            .arg("clipboard")
            .stdin(Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().is_ok() {
                return true;
            }
        }

        // Try xsel (X11)
        if let Ok(mut child) = Command::new("xsel")
            .arg("-b")
            .arg("-i")
            .stdin(Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().is_ok() {
                return true;
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().is_ok() {
                return true;
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(mut child) = Command::new("clip").stdin(Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().is_ok() {
                return true;
            }
        }
    }

    success
}

/// Read text from the system clipboard using arboard and native OS tool fallbacks.
pub fn get_clipboard_text() -> Option<String> {
    // 1. Arboard system clipboard
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if let Ok(s) = cb.get_text() {
            if !s.is_empty() {
                return Some(s);
            }
        }
    }

    // 2. Fallback CLI tools
    #[cfg(target_os = "linux")]
    {
        // Try wl-paste (Wayland)
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if let Ok(output) = Command::new("wl-paste").arg("--no-newline").output() {
                if output.status.success() {
                    if let Ok(s) = String::from_utf8(output.stdout) {
                        if !s.is_empty() {
                            return Some(s);
                        }
                    }
                }
            }
        }

        // Try xclip (X11)
        if let Ok(output) = Command::new("xclip")
            .arg("-selection")
            .arg("clipboard")
            .arg("-o")
            .output()
        {
            if output.status.success() {
                if let Ok(s) = String::from_utf8(output.stdout) {
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
        }

        // Try xsel (X11)
        if let Ok(output) = Command::new("xsel").arg("-b").arg("-o").output() {
            if output.status.success() {
                if let Ok(s) = String::from_utf8(output.stdout) {
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("pbpaste").output() {
            if output.status.success() {
                if let Ok(s) = String::from_utf8(output.stdout) {
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = Command::new("powershell")
            .args(["-NoProfile", "-Command", "Get-Clipboard"])
            .output()
        {
            if output.status.success() {
                if let Ok(s) = String::from_utf8(output.stdout) {
                    let trimmed = s.trim().to_string();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"Hello, world!"), "SGVsbG8sIHdvcmxkIQ==");
    }
}

/// An image taken off the system clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    /// Raw encoded bytes, as the clipboard held them.
    pub bytes: Vec<u8>,
    /// MIME type, e.g. `image/png`.
    pub media_type: String,
}

impl ClipboardImage {
    /// The `data:` URL an OpenAI-compatible server expects.
    pub fn to_data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, base64_encode(&self.bytes))
    }

    /// Pixel size, read out of the file's own header.
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        image_dimensions(&self.bytes)
    }
}

/// Width and height of a PNG, JPEG, GIF or BMP, from its header.
///
/// Only enough of each format is parsed to find the size: this is for a label
/// in the composer, not for decoding.
pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let be16 = |b: &[u8]| u16::from_be_bytes([b[0], b[1]]) as u32;

    // PNG: 8-byte signature, then an IHDR chunk whose data starts at 16.
    if bytes.len() > 24 && bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some((be32(&bytes[16..20]), be32(&bytes[20..24])));
    }
    // GIF: little-endian size at byte 6.
    if bytes.len() > 10 && bytes.starts_with(b"GIF") {
        let le16 = |b: &[u8]| u16::from_le_bytes([b[0], b[1]]) as u32;
        return Some((le16(&bytes[6..8]), le16(&bytes[8..10])));
    }
    // BMP: little-endian 32-bit size at byte 18.
    if bytes.len() > 26 && bytes.starts_with(b"BM") {
        let le32 = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        return Some((le32(&bytes[18..22]), le32(&bytes[22..26])));
    }
    // JPEG: walk the segments to a start-of-frame marker, which carries the size.
    if bytes.len() > 4 && bytes.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            // SOF0..SOF15, skipping the four that are not frame headers.
            if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                return Some((be16(&bytes[i + 7..i + 9]), be16(&bytes[i + 5..i + 7])));
            }
            let len = be16(&bytes[i + 2..i + 4]) as usize;
            if len < 2 {
                break;
            }
            i += 2 + len;
        }
    }
    None
}

/// The media type a file name implies, for the formats a vision model reads.
pub fn image_media_type(path: &std::path::Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// Read an image out of the system clipboard, if it holds one.
///
/// Goes through the platform's own tools rather than a decoding library: they
/// hand back the file exactly as it was copied, which is what the model wants
/// and what keeps a screenshot a screenshot.
pub fn get_clipboard_image() -> Option<ClipboardImage> {
    let attempts: &[(&str, &[&str], &str)] = if cfg!(target_os = "macos") {
        &[("pngpaste", &["-"], "image/png")]
    } else if cfg!(target_os = "windows") {
        &[]
    } else {
        &[
            ("wl-paste", &["--no-newline", "--type", "image/png"], "image/png"),
            ("xclip", &["-selection", "clipboard", "-t", "image/png", "-o"], "image/png"),
        ]
    };

    for (bin, args, media_type) in attempts {
        let Ok(out) = Command::new(bin).args(*args).stderr(Stdio::null()).output() else {
            continue;
        };
        if !out.status.success() || out.stdout.is_empty() {
            continue;
        }
        // A clipboard holding text answers these commands with the text, so
        // the bytes have to look like an image before they are believed.
        if image_dimensions(&out.stdout).is_none() {
            continue;
        }
        return Some(ClipboardImage { bytes: out.stdout, media_type: (*media_type).to_string() });
    }
    None
}

#[cfg(test)]
mod image_tests {
    use super::*;

    /// The smallest valid PNG: 1×1, transparent.
    fn tiny_png() -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v.extend_from_slice(&[0, 0, 0, 0]);
        v
    }

    #[test]
    fn a_png_says_how_big_it_is() {
        let mut png = tiny_png();
        png[16..20].copy_from_slice(&1440u32.to_be_bytes());
        png[20..24].copy_from_slice(&900u32.to_be_bytes());
        assert_eq!(image_dimensions(&png), Some((1440, 900)));
    }

    #[test]
    fn text_on_the_clipboard_is_not_mistaken_for_a_picture() {
        // wl-paste answers with the text when the clipboard holds text, so
        // the bytes are checked before they are believed.
        assert_eq!(image_dimensions(b"just some copied text"), None);
        assert_eq!(image_dimensions(b""), None);
        assert_eq!(image_dimensions(&[0x89, b'P', b'N', b'G']), None, "a truncated header is not a picture");
    }

    #[test]
    fn a_jpeg_and_a_gif_are_measured_too() {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        jpeg.extend_from_slice(b"JFIF\0");
        jpeg.extend_from_slice(&[0; 9]);
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        jpeg.extend_from_slice(&600u16.to_be_bytes());
        jpeg.extend_from_slice(&800u16.to_be_bytes());
        jpeg.extend_from_slice(&[0; 8]);
        assert_eq!(image_dimensions(&jpeg), Some((800, 600)));

        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&320u16.to_le_bytes());
        gif.extend_from_slice(&240u16.to_le_bytes());
        gif.extend_from_slice(&[0; 4]);
        assert_eq!(image_dimensions(&gif), Some((320, 240)));
    }

    #[test]
    fn a_clipboard_image_becomes_the_url_a_server_expects() {
        let img = ClipboardImage { bytes: vec![0, 1, 2], media_type: "image/png".into() };
        assert_eq!(img.to_data_url(), "data:image/png;base64,AAEC");
    }

    #[test]
    fn only_formats_a_model_can_read_are_offered() {
        use std::path::Path;
        assert_eq!(image_media_type(Path::new("shot.PNG")), Some("image/png"));
        assert_eq!(image_media_type(Path::new("a/b/photo.jpeg")), Some("image/jpeg"));
        assert_eq!(image_media_type(Path::new("notes.md")), None);
        assert_eq!(image_media_type(Path::new("noext")), None);
    }
}
