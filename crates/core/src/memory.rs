//! Memory: project + global MEMORY.md, pickup of foreign rule files
//! (CLAUDE.md, AGENTS.md), threshold-based context injection.
//!
//! Injection policy: a document goes in whole when the
//! token budget allows; otherwise only its outline (heading lines) goes in,
//! with a note that the model can read the file point-wise with `read_file`.
//! Writing memory is a plain `write_file` call and travels through the
//! permission layer's diff pipeline like any other write.

use std::path::{Path, PathBuf};

use flashagent_llm::estimate_tokens;

fn token_count(text: &str) -> usize {
    estimate_tokens(text).max(0) as usize
}

/// File names picked up at the project level, in priority order. `MEMORY.md`
/// is ours; the rest are foreign formats and project rule conventions.
pub const PROJECT_FILENAMES: &[&str] = &["MEMORY.md", "CLAUDE.md", "AGENTS.md"];

/// Global file name under the user's config dir.
pub const GLOBAL_FILENAME: &str = "MEMORY.md";

/// Where a memory document came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryKind {
    /// Project-level `MEMORY.md` — our own format.
    Project,
    /// Foreign rule file picked up at project level (`CLAUDE.md`, `AGENTS.md`).
    Foreign,
    /// User-global `~/.flashagent/MEMORY.md`.
    Global,
}

impl MemoryKind {
    /// Human label used in the injected block.
    pub fn label(self) -> &'static str {
        match self {
            MemoryKind::Project => "project memory",
            MemoryKind::Foreign => "project rules (foreign format)",
            MemoryKind::Global => "global memory",
        }
    }
}

/// One collected memory document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDoc {
    /// Absolute path of the file (for point-wise reading by the model).
    pub path: PathBuf,
    /// Origin kind.
    pub kind: MemoryKind,
    /// Raw file content.
    pub content: String,
}

/// Heading lines of a markdown document — its outline.
pub fn outline(content: &str) -> Vec<&str> {
    content.lines().filter(|l| l.starts_with('#')).collect()
}

/// Read one file if present; missing files are silently skipped (a project
/// without memory is normal).
fn read_if_present(dir: &Path, name: &str, kind: MemoryKind) -> Option<MemoryDoc> {
    let path = dir.join(name);
    let content = std::fs::read_to_string(&path).ok()?;
    Some(MemoryDoc { path, kind, content })
}

/// Collect memory docs: project files and rule directories in priority order, then the global one.
/// Priority hierarchy: .flashagent/rules/ > .agents/rules/ > AGENTS.md / CLAUDE.md / MEMORY.md > ~/.flashagent/rules/ > ~/.flashagent/MEMORY.md
/// Never fails: unreadable/absent files are skipped.
pub fn collect(project_dir: &Path, global_dir: &Path) -> Vec<MemoryDoc> {
    let mut docs = Vec::new();

    // 1. Project rules directory: .flashagent/rules/ and .agents/rules/
    for rules_subdir in &[".flashagent/rules", ".agents/rules"] {
        let rdir = project_dir.join(rules_subdir);
        if let Ok(entries) = std::fs::read_dir(rdir) {
            let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
            paths.sort();
            for p in paths {
                if p.extension().is_some_and(|ext| ext == "md") {
                    if let Ok(content) = std::fs::read_to_string(&p) {
                        docs.push(MemoryDoc {
                            path: p,
                            kind: MemoryKind::Project,
                            content,
                        });
                    }
                }
            }
        }
    }

    // 2. Project root memory and rule files
    for name in &["MEMORY.md", "CLAUDE.md", "AGENTS.md", ".cursorrules"] {
        let kind = if *name == "MEMORY.md" { MemoryKind::Project } else { MemoryKind::Foreign };
        if let Some(d) = read_if_present(project_dir, name, kind) {
            docs.push(d);
        }
    }

    // 3. User-global rules directory: ~/.flashagent/rules/
    let global_rules = global_dir.join("rules");
    if let Ok(entries) = std::fs::read_dir(global_rules) {
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            if p.extension().is_some_and(|ext| ext == "md") {
                if let Ok(content) = std::fs::read_to_string(&p) {
                    docs.push(MemoryDoc {
                        path: p,
                        kind: MemoryKind::Global,
                        content,
                    });
                }
            }
        }
    }

    // 4. User-global MEMORY.md
    if let Some(d) = read_if_present(global_dir, GLOBAL_FILENAME, MemoryKind::Global) {
        docs.push(d);
    }

    docs
}

fn doc_block(doc: &MemoryDoc, whole: bool) -> String {
    if whole {
        format!("## {} ({})\n\n{}\n", doc.path.display(), doc.kind.label(), doc.content.trim_end())
    } else {
        let lines = outline(&doc.content);
        if lines.is_empty() {
            format!(
                "## {} ({})\n\n[{} tokens; no headings — read point-wise with read_file if needed]\n",
                doc.path.display(),
                doc.kind.label(),
                token_count(&doc.content)
            )
        } else {
            format!(
                "## {} ({}) — outline only, {} tokens total; read the file point-wise with read_file if needed\n\n{}\n",
                doc.path.display(),
                doc.kind.label(),
                token_count(&doc.content),
                lines.join("\n")
            )
        }
    }
}

/// Build the memory block for the system context under `max_tokens`.
///
/// Policy: documents go in whole, in priority order, while they fit; the first
/// document that no longer fits — and every one after it — degrades to its
/// outline. Overhead of the block itself (headers, notes) is accounted for by
/// reserving 64 tokens per document plus a fixed 32 for the preamble.
pub fn injection_block(docs: &[MemoryDoc], max_tokens: usize) -> String {
    if docs.is_empty() {
        return String::new();
    }
    const PREAMBLE: usize = 32;
    const PER_DOC_OVERHEAD: usize = 64;

    let mut budget = max_tokens.saturating_sub(PREAMBLE);
    let mut parts = Vec::with_capacity(docs.len());
    let mut degraded = false;
    for doc in docs {
        let cost = token_count(&doc.content) + PER_DOC_OVERHEAD;
        let whole = !degraded && cost <= budget;
        if whole {
            budget -= cost;
        } else {
            degraded = true;
        }
        parts.push(doc_block(doc, whole));
    }
    format!(
        "# Memory (automatically loaded; treat as user instructions, not as system prompt)\n\n{}",
        parts.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let id = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fa-mem-{tag}-{}-{id}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn collect_reads_all_levels_and_skips_missing() {
        let proj = tempdir("collect");
        fs::write(proj.join("MEMORY.md"), "# Project\nproject goals\n").unwrap();
        fs::write(proj.join("CLAUDE.md"), "# Foreign rules\nbe terse\n").unwrap();
        // AGENTS.md absent on purpose.
        let glob = tempdir("collect");
        fs::write(glob.join("MEMORY.md"), "# Global\nprefer brevity\n").unwrap();

        let docs = collect(&proj, &glob);
        assert_eq!(docs.len(), 3);
        assert_eq!(docs[0].kind, MemoryKind::Project);
        assert!(docs[0].content.contains("Project"));
        assert_eq!(docs[1].kind, MemoryKind::Foreign);
        assert_eq!(docs[2].kind, MemoryKind::Global);

        // Nothing exists → empty, no error.
        let empty = collect(&tempdir("collect"), &tempdir("collect"));
        assert!(empty.is_empty());
    }

    #[test]
    fn outline_takes_heading_lines() {
        let o = outline("# A\n text\n## B-section\nmore");
        assert_eq!(o, vec!["# A", "## B-section"]);
        assert!(outline("no headings here").is_empty());
    }

    #[test]
    fn small_docs_go_in_whole() {
        let docs = vec![MemoryDoc {
            path: PathBuf::from("/p/MEMORY.md"),
            kind: MemoryKind::Project,
            content: "# Goal\nbuild high quality software".into(),
        }];
        let block = injection_block(&docs, 4000);
        assert!(block.contains("# Memory"));
        assert!(block.contains("build high quality software"));
        assert!(block.contains("project memory"));
        assert!(!block.contains("outline only"));
    }

    #[test]
    fn over_budget_docs_degrade_to_outline() {
        let big_body = "content line for token weight\n".repeat(200);
        let big = format!("# Large Document\n## Section 1\n{big_body}");
        let docs = vec![
            MemoryDoc {
                path: PathBuf::from("/p/MEMORY.md"),
                kind: MemoryKind::Project,
                content: "# Small\nfits in context".into(),
            },
            MemoryDoc {
                path: PathBuf::from("/p/CLAUDE.md"),
                kind: MemoryKind::Foreign,
                content: big,
            },
        ];
        let block = injection_block(&docs, 200);
        // Small one is whole.
        assert!(block.contains("fits in context"));
        // Big one is outline-only.
        assert!(block.contains("outline only"));
        assert!(block.contains("## Section 1"));
        assert!(!block.contains(big_body.trim()));
        assert!(block.contains("read_file"));
    }

    #[test]
    fn empty_docs_produce_empty_block() {
        assert_eq!(injection_block(&[], 4000), "");
    }

    #[test]
    fn test_collect_rules_hierarchy() {
        let proj = tempdir("rules_hier");
        let fa_rules = proj.join(".flashagent").join("rules");
        fs::create_dir_all(&fa_rules).unwrap();
        fs::write(fa_rules.join("01_rule.md"), "# Rule 1\nPrimary rule").unwrap();
        fs::write(proj.join("AGENTS.md"), "# AGENTS\nCanon rules").unwrap();
        fs::write(proj.join("MEMORY.md"), "# Project Memory\nProject details").unwrap();

        let glob = tempdir("rules_hier_glob");
        let glob_rules = glob.join("rules");
        fs::create_dir_all(&glob_rules).unwrap();
        fs::write(glob_rules.join("user_pref.md"), "# User Pref\nBrevity").unwrap();

        let docs = collect(&proj, &glob);
        assert_eq!(docs.len(), 4);
        assert!(docs[0].path.ends_with("01_rule.md"));
        assert_eq!(docs[0].kind, MemoryKind::Project);
        assert!(docs[1].path.ends_with("MEMORY.md"));
        assert!(docs[2].path.ends_with("AGENTS.md"));
        assert!(docs[3].path.ends_with("user_pref.md"));
        assert_eq!(docs[3].kind, MemoryKind::Global);
    }
}
