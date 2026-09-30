use std::sync::OnceLock;
use tiktoken_rs::CoreBPE;

use crate::types::estimate_tokens;

// A tokenizer that fails to load must not take the app down: this count is a
// status-line estimate, and the server corrects it with its own usage anyway.
static CL100K: OnceLock<Option<CoreBPE>> = OnceLock::new();
static O200K: OnceLock<Option<CoreBPE>> = OnceLock::new();

fn get_cl100k() -> Option<&'static CoreBPE> {
    CL100K
        .get_or_init(|| tiktoken_rs::cl100k_base().ok())
        .as_ref()
}

fn get_o200k() -> Option<&'static CoreBPE> {
    O200K
        .get_or_init(|| tiktoken_rs::o200k_base().ok())
        .as_ref()
}

/// `gpt-4o`, `o1`, `o3` use o200k_base; everything else cl100k_base. When that
/// table cannot be read, the estimate the server would replace anyway is used.
pub fn count_tokens(model_name: &str, text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let lower = model_name.to_lowercase();
    let bpe = if lower.contains("gpt-4o") || lower.contains("o1") || lower.contains("o3") {
        get_o200k()
    } else {
        get_cl100k()
    };
    count_with(bpe, text)
}

/// The one place the estimate stands in, split out so the fallback can be
/// tested directly instead of only through a table that happens to load.
fn count_with(bpe: Option<&CoreBPE>, text: &str) -> usize {
    match bpe {
        Some(bpe) => bpe.encode_ordinary(text).len(),
        None => estimate_tokens(text).max(0) as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_tokens() {
        assert_eq!(count_tokens("qwen2.5-coder", ""), 0);
        let tokens = count_tokens("qwen2.5-coder", "Hello, world!");
        assert!(tokens > 0);
        let tokens_o = count_tokens("gpt-4o", "Hello, world!");
        assert!(tokens_o > 0);
    }

    #[test]
    fn a_table_that_could_not_be_loaded_falls_back_to_the_estimate() {
        // The point of the change: None must count, not panic.
        let text = "Hello, world! Привет, мир!";
        let fallback = count_with(None, text);
        assert!(
            fallback > 0,
            "an estimate of a non-empty text is never zero"
        );
        // And it tracks the text the way the estimate is meant to.
        assert!(count_with(None, "a much longer sentence with many words") > fallback);
    }

    #[test]
    fn an_empty_text_is_zero_on_either_path() {
        assert_eq!(count_with(None, ""), 0);
        assert_eq!(count_tokens("qwen2.5-coder", ""), 0);
    }
}
