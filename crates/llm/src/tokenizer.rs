//! Model-aware token counting using official BPE tokenizers.

use std::sync::OnceLock;
use tiktoken_rs::CoreBPE;

static CL100K: OnceLock<CoreBPE> = OnceLock::new();
static O200K: OnceLock<CoreBPE> = OnceLock::new();

fn get_cl100k() -> &'static CoreBPE {
    CL100K.get_or_init(|| tiktoken_rs::cl100k_base().expect("failed to load cl100k_base"))
}

fn get_o200k() -> &'static CoreBPE {
    O200K.get_or_init(|| tiktoken_rs::o200k_base().expect("failed to load o200k_base"))
}

/// Tokenize `text` using the tokenizer strictly matching `model_name`.
/// - `gpt-4o`, `o1`, `o3`: o200k_base tokenizer.
/// - `qwen`, `llama`, `deepseek`, `mistral`, `claude`, `gpt-4`: cl100k_base tokenizer.
pub fn count_tokens(model_name: &str, text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let lower = model_name.to_lowercase();
    if lower.contains("gpt-4o") || lower.contains("o1") || lower.contains("o3") {
        let bpe = get_o200k();
        bpe.encode_ordinary(text).len()
    } else {
        let bpe = get_cl100k();
        bpe.encode_ordinary(text).len()
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
}
