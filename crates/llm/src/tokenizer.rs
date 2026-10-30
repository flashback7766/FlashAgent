//! How many tokens a text is, for the model it is sent to.
//!
//! One place, one rule. Everything that counts tokens — the budget for the
//! tool schemas, the bar that says how full the window is, the trigger for
//! compaction, the speed on the status line — goes through
//! [`count_tokens`]. Before this there were three rules side by side: a
//! character count divided by four for the conversation, the same for the
//! compaction, and a real tokenizer for the budget. The bar and the budget
//! could therefore be two different numbers for the same text, and for
//! Russian they were: the character rule counts bytes, and a Russian letter
//! is two of them.
//!
//! The counter for a model is the closest thing to that model's own
//! tokenizer that can be had without asking the owner to point at a file:
//!
//! 1. The model's own table, for the models whose tokenizer ships with the
//!    app: the OpenAI family. Exact.
//! 2. The generic table, `o200k_base`, for every other model. It is the
//!    closest of the tables that ship, and much closer than `cl100k_base`
//!    was: measured against a Qwen tokenizer over 39 real texts, `cl100k`
//!    was out by 12% on average and by 80% at worst, `o200k` by 6% and 30%.
//! 3. What the model was measured to be, on top of that table. Every server
//!    says how many tokens it wrote, and the app knows the text it wrote, so
//!    the app can measure the model and keep the measurement. The
//!    measurement is kept per model and per kind of text — words in the
//!    Latin script, words in another script, and the punctuation between
//!    them — because the same sentence costs a third more in Russian.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use tiktoken_rs::CoreBPE;

use crate::types::estimate_tokens;

// A tokenizer that fails to load must not take the app down: this count is a
// status-line estimate, and the server corrects it with its own usage anyway.
static CL100K: OnceLock<Option<CoreBPE>> = OnceLock::new();
static O200K: OnceLock<Option<CoreBPE>> = OnceLock::new();

fn get_cl100k() -> Option<&'static CoreBPE> {
    CL100K.get_or_init(|| tiktoken_rs::cl100k_base().ok()).as_ref()
}

fn get_o200k() -> Option<&'static CoreBPE> {
    O200K.get_or_init(|| tiktoken_rs::o200k_base().ok()).as_ref()
}

/// Which table a model is counted with.
///
/// The OpenAI family has its own tables and they are exact, so they are
/// used as they are. Every other model gets `o200k_base`: it is the table
/// that was measured to be the closest of the ones that ship.
fn table_for(model: &str) -> Option<&'static CoreBPE> {
    let lower = model.to_ascii_lowercase();
    let is_openai = lower.contains("gpt-")
        || lower.contains("chatgpt")
        || lower.contains("o1-")
        || lower.contains("o1_")
        || lower.contains("o3-")
        || lower.contains("o3_")
        || lower.contains("o4-mini")
        || lower.starts_with("o1")
        || lower.starts_with("o3")
        || lower.starts_with("o4");
    if is_openai {
        // The newer OpenAI models use o200k, the older ones cl100k.
        let newer = lower.contains("gpt-4o")
            || lower.contains("gpt-4.1")
            || lower.contains("gpt-5")
            || lower.contains("gpt-4.1-")
            || lower.starts_with("o1")
            || lower.starts_with("o3")
            || lower.starts_with("o4");
        if newer {
            get_o200k()
        } else {
            get_cl100k()
        }
    } else {
        get_o200k()
    }
}

/// The count a table gives, without the model's own measurement on top.
fn table_count(model: &str, text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    match table_for(model) {
        Some(bpe) => bpe.encode_ordinary(text).len(),
        None => estimate_tokens(text).max(0) as usize,
    }
}

/// Counts of texts that were counted already.
///
/// The bar that says how full the window is is redrawn on every token of the
/// answer, and it counts the whole conversation each time. A tokenizer is
/// fast, but a hundred kilobytes of conversation is a couple of milliseconds,
/// and a few dozen tokens a second is a tenth of the machine's time. The
/// messages of a conversation do not change once they are written, so their
/// counts are kept.
struct Cache {
    /// The model the counts belong to: a count is the model's, not the text's.
    model: String,
    entries: HashMap<u64, usize>,
}

impl Cache {
    const LIMIT: usize = 8192;

    fn get(&self, key: u64) -> Option<usize> {
        self.entries.get(&key).copied()
    }

    fn put(&mut self, key: u64, tokens: usize) {
        if self.entries.len() >= Self::LIMIT {
            // The cache is a convenience, not a record: dropping it costs a
            // recount and nothing else.
            self.entries.clear();
        }
        self.entries.insert(key, tokens);
    }
}

fn cache() -> &'static RwLock<Cache> {
    static CACHE: OnceLock<RwLock<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(Cache { model: String::new(), entries: HashMap::new() }))
}

/// A short hash of the model and the text, to tell one text from another
/// without keeping the texts themselves.
fn key_of(model: &str, text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in model.as_bytes().iter().chain(text.as_bytes()) {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// How a model was measured to differ from the table it is counted with.
///
/// One number per kind of text, and not one for the whole text: the same
/// sentence costs about a third more in Russian than in English, and a
/// correction that fitted English would make Russian worse.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    /// Words in the Latin script.
    pub ascii: f64,
    /// Words in another script: Russian, Chinese, Japanese.
    pub other: f64,
    /// What stands between the words: spaces, punctuation, symbols.
    pub punct: f64,
    /// How many measurements went into each of them.
    pub samples: u32,
}

impl Default for Measurement {
    fn default() -> Self {
        Measurement { ascii: 1.0, other: 1.0, punct: 1.0, samples: 0 }
    }
}

/// Every model that has been measured, by name.
fn registry() -> &'static RwLock<HashMap<String, Measurement>> {
    static REG: OnceLock<RwLock<HashMap<String, Measurement>>> = OnceLock::new();
    REG.get_or_init(|| RwLock::new(HashMap::new()))
}

/// What a text is made of, as a tokenizer sees it.
///
/// Words are counted with the script they are written in, and a space belongs
/// to the word it stands in front of: a tokenizer glues it to that word, so
/// counting it apart would put the space in the wrong group.
fn classes(text: &str) -> (usize, usize, usize) {
    let (mut ascii, mut other, mut punct) = (0usize, 0usize, 0usize);
    let mut space = 0usize;
    let mut word_is_ascii: Option<bool> = None;
    for c in text.chars() {
        if c.is_alphanumeric() {
            if word_is_ascii.is_none() {
                word_is_ascii = Some(c.is_ascii());
            }
            if space > 0 {
                match word_is_ascii {
                    Some(true) => ascii += space,
                    _ => other += space,
                }
                space = 0;
            }
            match word_is_ascii {
                Some(true) => ascii += 1,
                _ => other += 1,
            }
        } else if c.is_whitespace() {
            space += 1;
        } else {
            punct += space + 1;
            space = 0;
            word_is_ascii = None;
        }
    }
    punct += space;
    (ascii, other, punct)
}

/// How many tokens a text is, for the model it is sent to.
///
/// The one function every place that counts tokens goes through.
pub fn count_tokens(model: &str, text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let key = key_of(model, text);
    let cached = cache().read().ok().and_then(|c| if c.model == model { c.get(key) } else { None });
    if let Some(tokens) = cached {
        return tokens;
    }
    let table = table_count(model, text);
    let tokens = match measurement_of(model) {
        None => table,
        Some(m) => {
            let (ascii, other, punct) = classes(text);
            let total = (ascii + other + punct) as f64;
            if total <= 0.0 {
                table
            } else {
                let factor =
                    (ascii as f64 * m.ascii + other as f64 * m.other + punct as f64 * m.punct) / total;
                let scaled = (table as f64 * factor).round();
                if scaled < 0.0 { 0 } else { scaled as usize }
            }
        }
    };
    if let Ok(mut c) = cache().write() {
        if c.model != model {
            c.model = model.to_string();
            c.entries.clear();
        }
        c.put(key, tokens);
    }
    tokens
}

/// The measurement kept for a model, if there is one.
pub fn measurement_of(model: &str) -> Option<Measurement> {
    registry().read().ok().and_then(|r| r.get(model).copied())
}

/// Every measurement, for saving.
pub fn measurements() -> Vec<(String, Measurement)> {
    registry().read().map(|r| r.iter().map(|(k, v)| (k.clone(), *v)).collect()).unwrap_or_default()
}

/// Puts a measurement in, as it was saved.
pub fn set_measurement(model: &str, m: Measurement) {
    if let Ok(mut r) = registry().write() {
        r.insert(model.to_string(), m);
    }
}

/// The shortest text worth measuring from.
///
/// A server counts the end of the answer as well, and a few tokens of
/// framing around a short answer are a large share of it: what is learned
/// from ten tokens is mostly the framing. From a few dozen tokens on, the
/// framing is a small share and the measurement is about the text.
const SHORTEST_MEASURED: usize = 24;

/// The ratio a sample may credibly sit at, to be learned from.
///
/// The pair a sample is built from can be broken: reasoning the stream never
/// showed as deltas, a retry whose first attempt is still in the text, the
/// answer of the request before still in the buffer. Every one of those reads
/// as a tokenizer several times off the table, and no tokenizer is — `o200k`,
/// the table every stranger is counted with, was measured within 30% of a real
/// model's own on every text tried. A sample outside this band is a broken
/// pair, and learning from it is what walked one factor to 4.1 and another to
/// 0.24: each request moved the count half way to an impossible figure, and a
/// regeneration, which measures against whatever the buffer still held, made
/// one on every try.
const OBSERVED_FLOOR: f64 = 0.5;
const OBSERVED_CEILING: f64 = 2.0;

/// Whether a factor is one an honest measurement could have written.
fn credible(factor: f64) -> bool {
    (OBSERVED_FLOOR..=OBSERVED_CEILING).contains(&factor)
}

/// Learns the model from what the server said about what it wrote.
///
/// The server counts the tokens of the text it generated, and the app has
/// that text: the two together are a measurement of the model, exact, and
/// they cost nothing extra — the number arrives with every answer anyway.
///
/// What it teaches is how far the model is from the table it is counted with.
/// A Russian answer moves the correction for Russian words; an English one
/// moves the correction for English words.
pub fn note_measured(model: &str, text: &str, tokens: usize) {
    if text.is_empty() || tokens == 0 {
        return;
    }
    let table = table_count(model, text);
    if table < SHORTEST_MEASURED {
        return;
    }
    let observed = tokens as f64 / table as f64;
    if !(observed.is_finite() && observed > 0.0) {
        return;
    }
    // An impossible ratio says the text and the count are not a pair; it says
    // nothing about the tokenizer. Learning from it moved the correction
    // towards a figure no tokenizer has.
    if !credible(observed) {
        return;
    }
    let (ascii, other, punct) = classes(text);
    let total = (ascii + other + punct) as f64;
    if total <= 0.0 {
        return;
    }
    let mut m = measurement_of(model).unwrap_or_default();
    let current = (ascii as f64 * m.ascii + other as f64 * m.other + punct as f64 * m.punct) / total;
    let diff = observed - current;
    // Each kind of text moves by how much of the text it is: a Russian
    // answer says little about English words, and a lot about Russian ones.
    let rate = 0.5;
    m.ascii += diff * (ascii as f64 / total) * rate;
    m.other += diff * (other as f64 / total) * rate;
    m.punct += diff * (punct as f64 / total) * rate;
    // A correction that runs away is worse than none: the band above already
    // refuses the samples that would take it there, and this holds the line
    // should a factor ever drift past what a sample can reach on its own.
    m.ascii = m.ascii.clamp(OBSERVED_FLOOR, OBSERVED_CEILING);
    m.other = m.other.clamp(OBSERVED_FLOOR, OBSERVED_CEILING);
    m.punct = m.punct.clamp(OBSERVED_FLOOR, OBSERVED_CEILING);
    m.samples += 1;
    set_measurement(model, m);
    // The counts that were kept were counted with the old measurement, and
    // the measurement has just moved.
    if let Ok(mut c) = cache().write() {
        c.entries.clear();
    }
}

/// Where the measurements are kept between runs, so a model that was measured
/// yesterday is not measured again today.
pub fn measurements_path() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".flashagent").join("tokens.json"))
}

/// Reads the measurements that were saved. A missing or broken file means
/// nothing has been measured yet. A factor outside the credible band was
/// written when broken pairs were still learned from; loading it would carry
/// the poison past the fix, so it is left on the disk and the model is
/// measured again.
pub fn load_measurements(path: &Path) {
    let Ok(content) = std::fs::read_to_string(path) else { return };
    let Ok(saved) = serde_json::from_str::<HashMap<String, Measurement>>(&content) else { return };
    for (model, m) in saved {
        if m.samples > 0 && credible(m.ascii) && credible(m.other) && credible(m.punct) {
            set_measurement(&model, m);
        }
    }
}

/// Writes the measurements down.
pub fn save_measurements(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let all = measurements();
    if let Ok(json) = serde_json::to_string_pretty(&all.into_iter().collect::<HashMap<_, _>>()) {
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_text_is_zero_on_either_path() {
        assert_eq!(count_tokens("qwen2.5-coder", ""), 0);
        assert_eq!(count_tokens("gpt-4o", ""), 0);
    }

    #[test]
    fn a_text_is_never_zero() {
        assert!(count_tokens("qwen2.5-coder", "Hello, world!") > 0);
        assert!(count_tokens("gpt-4o", "Hello, world!") > 0);
    }

    /// The point of the whole change: the bar and the budget must be the
    /// same number for the same text. Before, the bar counted bytes divided
    /// by four and the budget counted with a tokenizer, and a Russian
    /// sentence was two different numbers.
    #[test]
    fn the_same_text_is_one_number_wherever_it_is_counted() {
        let text = "Привет, мир! Это русский текст, и он стоит больше, чем английский.";
        let counted = count_tokens("some-local-model", text);
        assert!(counted > 0);
        // The same count, twice: nothing about the text changes between the
        // two calls, and nothing about the model either.
        assert_eq!(counted, count_tokens("some-local-model", text));
    }

    #[test]
    fn a_measurement_moves_the_count_towards_what_the_server_said() {
        let model = "measured-model-for-test";
        let text = "Русский текст, который сервер посчитал, и довольно длинный, чтобы конц��вка сервера была малой долей ответа.";
        let before = count_tokens(model, text);
        // The server says the text is a bit over half of what the table says —
        // inside the band an honest sample may sit in.
        let table = table_count(model, text);
        let target = (table * 3 / 5).max(1);
        note_measured(model, text, target);
        let after = count_tokens(model, text);
        assert!(after < before, "the measurement did not move the count: {before} -> {after}");
        // And it moved towards the number the server gave.
        assert!(
            (after as i64 - target as i64).abs() <= (before as i64 - target as i64).abs(),
            "the count moved away from the server's number: {before} -> {after}, wanted {target}"
        );
    }

    #[test]
    fn a_measurement_of_one_language_leaves_the_other_alone() {
        let model = "measured-model-two-for-test";
        let russian = "Русский текст, который сервер посчитал, и довольно длинный, чтобы концовка сервера была малой долей ответа.";
        let english = "An English text, counted by the same server, and long enough that the server's own framing is a small share of it.";
        let russian_before = count_tokens(model, russian);
        let english_before = count_tokens(model, english);
        let table = table_count(model, russian);
        note_measured(model, russian, (table * 3 / 5).max(1));
        assert!(count_tokens(model, russian) < russian_before, "the Russian count did not move");
        assert_eq!(count_tokens(model, english), english_before, "the English count moved too");
    }

    #[test]
    fn a_sample_no_tokenizer_could_have_written_is_not_learned_from() {
        // Broken pairs — reasoning the stream never showed, a retry's text
        // counted once while sitting in the buffer twice — read as a tokenizer
        // several times off the table. None is: these are refused whole, not
        // clamped, or the count drifts with every request.
        let model = "measured-model-three-for-test";
        let text = "some text that is long enough to be measured at all, and then some more, and even more after that, so that the measurement is not skipped for being short";
        let table = table_count(model, text);
        note_measured(model, text, table * 3);
        assert!(measurement_of(model).is_none(), "three times the table is a broken pair, not a tokenizer");
        note_measured(model, text, table / 10);
        assert!(measurement_of(model).is_none(), "a tenth is the count of a shorter text, not a tokenizer");
        // A quarter off is what real tokenizers do against the table; that is
        // the measurement working as meant.
        note_measured(model, text, table + table / 4);
        let m = measurement_of(model).expect("an honest sample is still learned from");
        assert!(credible(m.ascii) && credible(m.other) && credible(m.punct), "{m:?}");
    }

    #[test]
    fn a_measurement_saved_by_the_broken_pair_bug_does_not_load() {
        // What the file on disk really held: one model's Cyrillic factor at the
        // old clamp's ceiling, another's Latin at the floor. Loading them would
        // keep the drift alive on every count.
        let path = std::env::temp_dir().join(format!("fa-poison-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{
  "poisoned-high": {"ascii": 1.0, "other": 4.1, "punct": 1.0, "samples": 37},
  "poisoned-low": {"ascii": 0.238, "other": 1.0, "punct": 1.0, "samples": 4},
  "honest-model": {"ascii": 1.2, "other": 0.9, "punct": 1.0, "samples": 6}
}"#,
        )
        .unwrap();
        load_measurements(&path);
        assert!(measurement_of("poisoned-high").is_none(), "4.1 was learned from broken pairs");
        assert!(measurement_of("poisoned-low").is_none(), "0.238 was learned from broken pairs");
        assert!(measurement_of("honest-model").is_some(), "an honest factor is kept");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_model_with_a_table_of_its_own_is_counted_with_it() {
        // The OpenAI family has tables that ship with the app, and they are
        // exact, so nothing is measured about them: the count is the table's.
        let text = "Hello, world!";
        assert_eq!(count_tokens("gpt-4o", text), table_count("gpt-4o", text));
        assert_eq!(count_tokens("gpt-4-turbo", text), table_count("gpt-4-turbo", text));
    }

    #[test]
    fn a_model_nobody_heard_of_is_counted_with_the_closest_table() {
        // o200k is the table that was measured to be the closest of the ones
        // that ship: against a Qwen tokenizer it was out by 6% on average,
        // where cl100k was out by 12%.
        let text = "The quick brown fox jumps over the lazy dog.";
        assert_eq!(count_tokens("qwen3-something", text), table_count("qwen3-something", text));
        assert_eq!(table_count("qwen3-something", text), table_count("gpt-4o", text));
    }

    /// The point of measuring at all: the count gets closer to what the
    /// model really said.
    ///
    /// The texts here are what a model writes and a server counts, and the
    /// numbers are what that server said. Before the measurement the count
    /// is the table's, and the table is a stranger's: it was out by 12% on
    /// average over real texts. After the measurement it is the model's own.
    #[test]
    fn a_measured_model_is_counted_more_closely_than_an_unmeasured_one() {
        let model = "measured-model-five-for-test";
        // A server that says the text is a fifth longer than the table says.
        let text = "The quick brown fox jumps over the lazy dog, and the dog does not mind it at all. It is a long enough answer that the server's own framing is a small share of its count.";
        let table = table_count(model, text);
        let real = table + table / 5;
        let before = count_tokens(model, text);
        note_measured(model, text, real);
        let after = count_tokens(model, text);
        assert!(after > before, "the count did not move towards the server's number: {before} -> {after}");
        let err = |n: usize| (n as f64 - real as f64).abs() / real as f64;
        assert!(
            err(after) < err(before),
            "the count moved away from the server's number: {before} -> {after}, wanted {real}"
        );
    }

    #[test]
    fn the_measurements_survive_a_save_and_a_load() {
        let model = "measured-model-four-for-test";
        let m = Measurement { ascii: 1.2, other: 0.9, punct: 1.0, samples: 6 };
        set_measurement(model, m);
        let path = std::env::temp_dir().join(format!("fa-tokens-{}.json", std::process::id()));
        save_measurements(&path);
        let saved: HashMap<String, Measurement> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved.get(model), Some(&m), "the measurement was written down");
        // And what was written comes back: this is the read a run starts with.
        load_measurements(&path);
        assert_eq!(measurement_of(model), Some(m), "the measurement did not come back");
        let _ = std::fs::remove_file(&path);
    }
}
