//! File symbol and structural outline extractor (`outline_file`).
//!
//! Provides a concise, token-efficient summary of functions, structs, classes,
//! traits, and headings across Rust, Python, TS/JS, Go, C/C++, and Markdown files.

use std::path::Path;
use crate::ToolError;

/// Outlines structural symbols from the target file with 1-based line numbers.
pub fn outline_file(cwd: &Path, rel_path: &str) -> Result<String, ToolError> {
    let full = cwd.join(rel_path);
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
                trimmed.starts_with("fn ")
                    || trimmed.starts_with("pub fn ")
                    || trimmed.starts_with("async fn ")
                    || trimmed.starts_with("pub async fn ")
                    || trimmed.starts_with("struct ")
                    || trimmed.starts_with("pub struct ")
                    || trimmed.starts_with("enum ")
                    || trimmed.starts_with("pub enum ")
                    || trimmed.starts_with("trait ")
                    || trimmed.starts_with("pub trait ")
                    || trimmed.starts_with("impl ")
                    || trimmed.starts_with("pub type ")
                    || trimmed.starts_with("type ")
                    || trimmed.starts_with("mod ")
                    || trimmed.starts_with("pub mod ")
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
            // Cut trailing block openers or semicolons for brevity
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
