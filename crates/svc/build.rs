use std::process::Command;

fn is_version_tag(tag: &str) -> bool {
    let lower = tag.to_lowercase();
    if lower == "beta" || lower == "release" || lower == "stable" || lower == "latest" || lower == "nightly" {
        return false;
    }
    (tag.starts_with('b') && tag[1..].chars().all(|c| c.is_ascii_digit()))
        || (tag.starts_with('v') && tag.len() > 1 && tag[1..].chars().next().is_some_and(|c| c.is_ascii_digit()))
        || tag.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn main() {
    println!("cargo:rerun-if-env-changed=FLASHAGENT_VERSION");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/tags");
    println!("cargo:rerun-if-changed=../../packaging/arch/PKGBUILD");

    if let Ok(val) = std::env::var("FLASHAGENT_VERSION") {
        if is_version_tag(&val) {
            return;
        }
    }

    // 1. Check all tags pointing at HEAD for a concrete version tag (e.g. b215, v0.1.0)
    if let Ok(output) = Command::new("git")
        .args(["tag", "--points-at", "HEAD"])
        .output()
    {
        if output.status.success() {
            let tags = String::from_utf8_lossy(&output.stdout);
            for line in tags.lines() {
                let tag = line.trim();
                if is_version_tag(tag) {
                    println!("cargo:rustc-env=FLASHAGENT_VERSION={tag}");
                    return;
                }
            }
        }
    }

    // 2. Check tags matching build pattern 'b[0-9]*' or 'v*'
    for pattern in &["b[0-9]*", "v[0-9]*"] {
        if let Ok(output) = Command::new("git")
            .args(["describe", "--tags", "--match", pattern])
            .output()
        {
            if output.status.success() {
                let tag = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let clean = tag.split('-').next().unwrap_or(&tag);
                if is_version_tag(clean) {
                    println!("cargo:rustc-env=FLASHAGENT_VERSION={clean}");
                    return;
                }
            }
        }
    }

    // 3. Fallback: check packaging/arch/PKGBUILD pkgver
    let pkgbuild_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/arch/PKGBUILD");
    if let Ok(content) = std::fs::read_to_string(&pkgbuild_path) {
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("pkgver=") {
                let ver = rest.trim().trim_matches('"').trim_matches('\'');
                if is_version_tag(ver) {
                    println!("cargo:rustc-env=FLASHAGENT_VERSION={ver}");
                    return;
                }
            }
        }
    }

    // 4. Default fallback: b218
    println!("cargo:rustc-env=FLASHAGENT_VERSION=b218");
}
