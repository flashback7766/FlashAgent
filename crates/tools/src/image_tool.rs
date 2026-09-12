//! `view_image` — let the model look at a picture in the project.
//!
//! Reading a PNG as text tells the model nothing. A vision model can read the
//! diagram in `docs/`, the screenshot attached to a bug report, or the mockup
//! it is being asked to build — but only if the file reaches it as an image,
//! so the result of this call is the picture itself.

use std::path::{Path, PathBuf};

use flashagent_core::base64_encode;
use flashagent_core::ToolOutput;
use serde::Deserialize;

/// Bigger than this and the request becomes the problem: base64 adds a third
/// again, and a model's image budget is measured in hundreds of tokens.
const MAX_BYTES: usize = 8 * 1024 * 1024;

/// Arguments for `view_image`.
#[derive(Debug, Deserialize)]
pub struct ViewImageArgs {
    /// Path to the image, relative to the working directory or absolute.
    pub path: String,
}

/// Media type for the formats a vision model reads.
fn media_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// Width and height from the file's own header, when it is a format whose
/// size is easy to read. Only used for the line the user sees.
fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    if bytes.len() > 24 && bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some((be32(&bytes[16..20]), be32(&bytes[20..24])));
    }
    if bytes.len() > 10 && bytes.starts_with(b"GIF") {
        let le16 = |b: &[u8]| u16::from_le_bytes([b[0], b[1]]) as u32;
        return Some((le16(&bytes[6..8]), le16(&bytes[8..10])));
    }
    None
}

/// Resolve a path against the working directory, refusing to leave it.
fn resolve(cwd: &Path, raw: &str) -> Result<PathBuf, String> {
    let joined = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        cwd.join(raw)
    };
    let canonical = joined
        .canonicalize()
        .map_err(|_| format!("no such file: {raw}"))?;
    let root = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    if !canonical.starts_with(&root) {
        return Err(format!(
            "{raw} is outside the working directory; images are read from the project only"
        ));
    }
    Ok(canonical)
}

/// Open an image so the model can see it.
pub fn view_image(cwd: &Path, args_json: &str, vision_supported: bool) -> ToolOutput {
    let fail = |msg: String| ToolOutput { content: msg, is_error: true, images: Vec::new() };

    let args: ViewImageArgs = match serde_json::from_str(args_json) {
        Ok(a) => a,
        Err(e) => return fail(format!("view_image: bad arguments: {e}")),
    };

    // Saying this plainly beats sending a picture into a model that will
    // describe its own confusion.
    if !vision_supported {
        return fail(
            "view_image: the current model cannot see images. Tell the user which file you \
             wanted to look at and ask them to switch to a vision model (F3)."
                .to_string(),
        );
    }

    let path = match resolve(cwd, &args.path) {
        Ok(p) => p,
        Err(e) => return fail(format!("view_image: {e}")),
    };
    let Some(media) = media_type(&path) else {
        return fail(format!(
            "view_image: {} is not an image format a model can read (png, jpg, gif, webp, bmp)",
            args.path
        ));
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => return fail(format!("view_image: could not read {}: {e}", args.path)),
    };
    if bytes.is_empty() {
        return fail(format!("view_image: {} is empty", args.path));
    }
    if bytes.len() > MAX_BYTES {
        return fail(format!(
            "view_image: {} is {:.1} MB, over the {} MB limit — resize it or point at a smaller copy",
            args.path,
            bytes.len() as f64 / (1024.0 * 1024.0),
            MAX_BYTES / (1024 * 1024)
        ));
    }

    let size = dimensions(&bytes)
        .map(|(w, h)| format!(" ({w}×{h})"))
        .unwrap_or_default();
    ToolOutput {
        content: format!(
            "Opened {}{size}. The image follows this result — describe what you actually see in it.",
            args.path
        ),
        is_error: false,
        images: vec![format!("data:{media};base64,{}", base64_encode(&bytes))],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v
    }

    #[test]
    fn an_image_comes_back_as_an_image() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("diagram.png"), png(1440, 900)).unwrap();
        let out = view_image(dir.path(), r#"{"path":"diagram.png"}"#, true);
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("1440×900"), "{}", out.content);
        assert_eq!(out.images.len(), 1);
        assert!(out.images[0].starts_with("data:image/png;base64,"));
    }

    #[test]
    fn a_model_that_cannot_see_is_told_so_instead_of_being_shown() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("d.png"), png(2, 2)).unwrap();
        let out = view_image(dir.path(), r#"{"path":"d.png"}"#, false);
        assert!(out.is_error);
        assert!(out.images.is_empty(), "nothing is sent into a model that cannot read it");
        assert!(out.content.contains("cannot see images"), "{}", out.content);
    }

    #[test]
    fn a_file_that_is_not_an_image_is_refused_by_its_own_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.md"), "# hello").unwrap();
        let out = view_image(dir.path(), r#"{"path":"notes.md"}"#, true);
        assert!(out.is_error);
        assert!(out.content.contains("not an image format"), "{}", out.content);
    }

    #[test]
    fn nothing_outside_the_project_is_opened() {
        // The same boundary every other file tool keeps: a tool call is not a
        // way to read /home/user/.ssh or a screenshot from someone's desktop.
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.png"), png(1, 1)).unwrap();
        let path = outside.path().join("secret.png");
        // Built with serde, not format!: a Windows path is full of backslashes,
        // which are escapes inside a JSON string.
        let args = serde_json::json!({ "path": path.to_string_lossy() }).to_string();
        let out = view_image(dir.path(), &args, true);
        assert!(out.is_error);
        assert!(out.content.contains("outside the working directory"), "{}", out.content);
    }

    #[test]
    fn a_missing_file_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let out = view_image(dir.path(), r#"{"path":"ghost.png"}"#, true);
        assert!(out.is_error);
        assert!(out.content.contains("no such file"), "{}", out.content);
    }

    #[test]
    fn something_too_big_to_send_is_refused_with_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let mut big = png(10, 10);
        big.resize(MAX_BYTES + 1024, 0);
        std::fs::write(dir.path().join("huge.png"), &big).unwrap();
        let out = view_image(dir.path(), r#"{"path":"huge.png"}"#, true);
        assert!(out.is_error);
        assert!(out.content.contains("over the"), "{}", out.content);
        assert!(out.images.is_empty());
    }
}
