//! `outline_file`: functions, types and headings with line numbers, for Rust,
//! Python, TS/JS, Go, C/C++ and Markdown.

use std::path::Path;
use crate::ToolError;

pub fn outline_file(cwd: &Path, rel_path: &str) -> Result<String, ToolError> {
    let full = flashagent_core::resolve_path(cwd, rel_path);
    if !full.exists() {
        return Err(ToolError::Other(format!("file not found: {rel_path}")));
    }
    if full.is_dir() {
        return Err(ToolError::Other(format!("path is a directory, not a file: {rel_path}")));
    }

    let content = std::fs::read_to_string(&full)
        .map_err(|e| ToolError::Other(format!("failed to read file '{rel_path}': {e}")))?;

    let mut lines_out = Vec::new();
    let ext = full.extension().and_then(|s| s.to_str()).unwrap_or("");

    for (idx, raw_line) in content.lines().enumerate() {
        let line_num = idx + 1;
        let line = raw_line.trim_end();
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        let is_symbol = match ext {
            "rs" => {
                // A declaration is read past its visibility and modifiers: `pub(crate)`,
                // `pub(super)`, `unsafe`, `async`, `const`. The old list named whole
                // prefixes, so half the functions in this repository were invisible to
                // the outline while it still said "N symbols found".
                let mut bare = trimmed;
                bare = strip_visibility(bare);
                for modifier in ["unsafe ", "async ", "const ", "default ", "extern "] {
                    if let Some(rest) = bare.strip_prefix(modifier) {
                        // `extern "C" fn` keeps a string in the middle.
                        let rest = rest.trim_start();
                        let rest = rest.strip_prefix('"').and_then(|r| r.split_once('"').map(|(_, after)| after)).unwrap_or(rest).trim_start();
                        bare = rest;
                    }
                }
                bare.starts_with("fn ")
                    || bare.starts_with("struct ")
                    || bare.starts_with("enum ")
                    || bare.starts_with("trait ")
                    || bare.starts_with("union ")
                    || bare.starts_with("type ")
                    || bare.starts_with("mod ")
                    || bare.starts_with("macro_rules!")
                    || bare.starts_with("impl ") || bare.starts_with("impl<")
            }
            "py" => {
                trimmed.starts_with("def ")
                    || trimmed.starts_with("async def ")
                    || trimmed.starts_with("class ")
                    || trimmed.starts_with("@")
            }
            "js" | "jsx" | "ts" | "tsx" => {
                trimmed.starts_with("function ")
                    || trimmed.starts_with("export function ")
                    || trimmed.starts_with("async function ")
                    || trimmed.starts_with("export async function ")
                    || trimmed.starts_with("class ")
                    || trimmed.starts_with("export class ")
                    || trimmed.starts_with("interface ")
                    || trimmed.starts_with("export interface ")
                    || trimmed.starts_with("type ")
                    || trimmed.starts_with("export type ")
                    || (trimmed.starts_with("export const ") && (trimmed.contains("=>") || trimmed.contains("function")))
            }
            "go" => {
                trimmed.starts_with("func ")
                    || trimmed.starts_with("type ")
                    || trimmed.starts_with("package ")
            }
            "md" | "markdown" => {
                trimmed.starts_with('#')
            }
            _ => {
                trimmed.starts_with("class ")
                    || trimmed.starts_with("struct ")
                    || trimmed.starts_with("enum ")
                    || trimmed.starts_with("fn ")
                    || trimmed.starts_with("def ")
                    || trimmed.starts_with("func ")
                    || trimmed.starts_with('#')
            }
        };

        if is_symbol {
            let clean = trimmed
                .trim_end_matches('{')
                .trim_end_matches(';')
                .trim();
            lines_out.push(format!("L{line_num:4}: {clean}"));
        }
    }

    if lines_out.is_empty() {
        let total_lines = content.lines().count();
        return Ok(format!("File '{rel_path}' ({total_lines} lines): no structural symbols detected."));
    }

    let summary = format!("Outline for '{}' ({} symbols found):\n{}", rel_path, lines_out.len(), lines_out.join("\n"));
    Ok(summary)
}

/// `pub`, `pub(crate)`, `pub(super)`, `pub(in path)` — the declaration follows.
fn strip_visibility(line: &str) -> &str {
    let Some(rest) = line.strip_prefix("pub") else { return line };
    let rest = rest.trim_start();
    if let Some(after_paren) = rest.strip_prefix('(').and_then(|r| r.split_once(')')) {
        return after_paren.1.trim_start();
    }
    rest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_outline_rust_file() {
        let temp = crate::testing::tempdir();
        let file = temp.join("main.rs");
        std::fs::write(
            &file,
            r#"
use std::io;

pub struct User {
    name: String,
}

impl User {
    pub fn new(name: String) -> Self {
        Self { name }
    }

    fn private_helper(&self) -> bool {
        true
    }
}

pub enum Status {
    Active,
    Inactive,
}
"#,
        )
        .unwrap();

        let res = outline_file(&temp, "main.rs").unwrap();
        assert!(res.contains("pub struct User"));
        assert!(res.contains("impl User"));
        assert!(res.contains("pub fn new(name: String) -> Self"));
        assert!(res.contains("fn private_helper(&self) -> bool"));
        assert!(res.contains("pub enum Status"));
    }

    #[test]
    fn hidden_and_generic_declarations_are_found() {
        let temp = crate::testing::tempdir();
        let file = temp.join("lib.rs");
        std::fs::write(
            &file,
            "pub(crate) fn hidden() {}\n\
             pub(super) fn near() {}\n\
             unsafe fn danger() {}\n\
             const fn built() -> u8 { 1 }\n\
             pub const fn visible() -> u8 { 1 }\n\
             impl<T> Foo for T {}\n\
             impl<'de> Deserialize<'de> for X {}\n\
             pub union Bits { a: u32 }\n",
        )
        .unwrap();
        let res = outline_file(&temp, "lib.rs").unwrap();
        for name in ["hidden", "near", "danger", "built", "visible", "impl<T>", "impl<'de>", "union Bits"] {
            assert!(res.contains(name), "{name} is missing from the outline:\n{res}");
        }
    }

    #[test]
    fn test_outline_python_file() {
        let temp = crate::testing::tempdir();
        let file = temp.join("app.py");
        std::fs::write(
            &file,
            r#"
class Agent:
    def __init__(self, name):
        self.name = name

    async def run(self):
        pass
"#,
        )
        .unwrap();

        let res = outline_file(&temp, "app.py").unwrap();
        assert!(res.contains("class Agent:"));
        assert!(res.contains("def __init__(self, name):"));
        assert!(res.contains("async def run(self):"));
    }
}
