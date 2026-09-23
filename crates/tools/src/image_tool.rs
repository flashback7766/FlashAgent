//! `view_image`: the result is the picture itself, so a vision model can read
//! a diagram, screenshot or mockup in the project.

use std::path::{Path, PathBuf};

use flashagent_core::base64_encode;
use flashagent_core::ToolOutput;
use serde::Deserialize;

/// Base64 adds a third, and a model's image budget is hundreds of tokens.
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct ViewImageArgs {
    /// Relative to the working directory, or absolute.
    pub path: String,
}

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

/// From the file header, for the line the user sees.
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

/// Refuses to leave the working directory.
fn resolve(cwd: &Path, raw: &str) -> Result<PathBuf, String> {
    let joined = flashagent_core::resolve_path(cwd, raw);
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

pub fn view_image(cwd: &Path, args_json: &str, vision_supported: bool) -> ToolOutput {
    let fail = |msg: String| ToolOutput { content: msg, is_error: true, images: Vec::new() };

    // Through the same resolver as the approval card and the permissions, so
    // `file_path` is read as the path they showed.
    let Some(value) = flashagent_llm::effective_args(args_json, "view_image") else {
        return fail("view_image: bad arguments: not a JSON object".to_string());
    };
    let args: ViewImageArgs = match serde_json::from_value(value) {
        Ok(a) => a,
        Err(e) => return fail(format!("view_image: bad arguments: {e}")),
    };

    // Said plainly instead of sending a picture to a model that cannot see it.
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
    fn a_path_given_as_file_path_is_the_one_approved() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("d.png"), png(2, 2)).unwrap();
        let out = view_image(dir.path(), r#"{"file_path":"d.png"}"#, true);
        assert!(!out.is_error, "{}", out.content);
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
        // The same boundary as every other file tool.
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.png"), png(1, 1)).unwrap();
        let path = outside.path().join("secret.png");
        // serde, not format!: Windows backslashes are escapes in JSON strings.
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
