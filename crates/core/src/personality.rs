//! How FlashAgent talks: a base style and a few characteristics on top.
//!
//! The choice only ever reaches the model as a short section of the system
//! prompt about tone and presentation. It never changes what the agent does
//! — tool discipline, honesty about failures and the quality of the code are
//! the same in every style — and "Default" everywhere adds nothing at all, so
//! a user who never opens the screen gets exactly the prompt they had before.

use serde::{Deserialize, Serialize};

/// The overall voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseStyle {
    #[default]
    Default,
    Professional,
    Friendly,
    Candid,
    Quirky,
    Efficient,
    Cynical,
}

impl BaseStyle {
    pub const ALL: [BaseStyle; 7] = [
        BaseStyle::Default,
        BaseStyle::Professional,
        BaseStyle::Friendly,
        BaseStyle::Candid,
        BaseStyle::Quirky,
        BaseStyle::Efficient,
        BaseStyle::Cynical,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Professional => "Professional",
            Self::Friendly => "Friendly",
            Self::Candid => "Candid",
            Self::Quirky => "Quirky",
            Self::Efficient => "Efficient",
            Self::Cynical => "Cynical",
        }
    }

    /// The one-line description shown under the name.
    pub fn blurb(self) -> &'static str {
        match self {
            Self::Default => "Preset style and tone",
            Self::Professional => "Polished and precise",
            Self::Friendly => "Warm and chatty",
            Self::Candid => "Direct and encouraging",
            Self::Quirky => "Playful and imaginative",
            Self::Efficient => "Concise and plain",
            Self::Cynical => "Critical and sarcastic",
        }
    }

    fn instruction(self) -> Option<&'static str> {
        Some(match self {
            Self::Default => return None,
            Self::Professional => "Sound polished and precise: measured wording, exact terms, no slang or jokes.",
            Self::Friendly => "Sound warm and chatty, like a friendly colleague pairing with the user.",
            Self::Candid => "Be direct and encouraging: say plainly what is wrong or risky, and what is good.",
            Self::Quirky => "Be playful and imaginative: a light touch of humour and vivid comparisons are welcome.",
            Self::Efficient => "Be concise and plain: the fewest words that carry the answer, no pleasantries.",
            Self::Cynical => {
                "Be critical and a little sarcastic about bad ideas and fragile code, never about the user; \
                 the sarcasm must not replace a clear answer."
            }
        })
    }

    /// The style after (or, with `forward` false, before) this one.
    pub fn cycle(self, forward: bool) -> Self {
        let i = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        let n = Self::ALL.len();
        Self::ALL[if forward { (i + 1) % n } else { (i + n - 1) % n }]
    }
}

/// More, the default, or less of a characteristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Less,
    #[default]
    Default,
    More,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Self::Less => "Less",
            Self::Default => "Default",
            Self::More => "More",
        }
    }

    /// Less → Default → More → Less, or the other way.
    pub fn cycle(self, forward: bool) -> Self {
        match (self, forward) {
            (Self::Less, true) | (Self::More, false) => Self::Default,
            (Self::Default, true) | (Self::Less, false) => Self::More,
            (Self::More, true) | (Self::Default, false) => Self::Less,
        }
    }
}

/// A characteristic adjustable on top of the base style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trait {
    Warm,
    Enthusiastic,
    HeadersAndLists,
    Emoji,
}

impl Trait {
    pub const ALL: [Trait; 4] = [Trait::Warm, Trait::Enthusiastic, Trait::HeadersAndLists, Trait::Emoji];

    pub fn label(self) -> &'static str {
        match self {
            Self::Warm => "Warm",
            Self::Enthusiastic => "Enthusiastic",
            Self::HeadersAndLists => "Headers & Lists",
            Self::Emoji => "Emoji",
        }
    }

    /// What `level` of this means, as shown to the user.
    pub fn blurb(self, level: Level) -> &'static str {
        match (self, level) {
            (_, Level::Default) => "",
            (Self::Warm, Level::More) => "Friendlier and more personable",
            (Self::Warm, Level::Less) => "More professional and factual",
            (Self::Enthusiastic, Level::More) => "More energy and excitement",
            (Self::Enthusiastic, Level::Less) => "Calmer and more neutral",
            (Self::HeadersAndLists, Level::More) => "Use clear formatting and lists",
            (Self::HeadersAndLists, Level::Less) => "More paragraphs instead of lists",
            (Self::Emoji, Level::More) => "Use more emoji",
            (Self::Emoji, Level::Less) => "Don't use as many emoji",
        }
    }

    fn instruction(self, level: Level) -> Option<&'static str> {
        Some(match (self, level) {
            (_, Level::Default) => return None,
            (Self::Warm, Level::More) => "Be warmer and more personable than usual.",
            (Self::Warm, Level::Less) => "Keep the warmth down: professional and factual.",
            (Self::Enthusiastic, Level::More) => "Show more energy and excitement.",
            (Self::Enthusiastic, Level::Less) => "Stay calm and neutral; no excitement.",
            (Self::HeadersAndLists, Level::More) => "Structure answers with headers and lists where they help.",
            (Self::HeadersAndLists, Level::Less) => "Prefer flowing paragraphs over headers and bullet lists.",
            (Self::Emoji, Level::More) => "Use emoji where they fit naturally.",
            (Self::Emoji, Level::Less) => "Do not use emoji.",
        })
    }
}

/// The user's choice of voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Personality {
    pub base: BaseStyle,
    pub warm: Level,
    pub enthusiastic: Level,
    pub headers_lists: Level,
    pub emoji: Level,
}

impl Personality {
    pub fn level(&self, t: Trait) -> Level {
        match t {
            Trait::Warm => self.warm,
            Trait::Enthusiastic => self.enthusiastic,
            Trait::HeadersAndLists => self.headers_lists,
            Trait::Emoji => self.emoji,
        }
    }

    pub fn level_mut(&mut self, t: Trait) -> &mut Level {
        match t {
            Trait::Warm => &mut self.warm,
            Trait::Enthusiastic => &mut self.enthusiastic,
            Trait::HeadersAndLists => &mut self.headers_lists,
            Trait::Emoji => &mut self.emoji,
        }
    }

    /// The system-prompt section for this choice; `None` when everything is
    /// at its default, so the prompt stays exactly as it was.
    pub fn prompt_section(&self) -> Option<String> {
        let lines: Vec<&str> = self
            .base
            .instruction()
            .into_iter()
            .chain(Trait::ALL.iter().filter_map(|t| t.instruction(self.level(*t))))
            .collect();
        if lines.is_empty() {
            return None;
        }
        Some(format!(
            "# Style and tone (the user's choice)\n\
             Apply this to how you write replies to the user. It changes tone and presentation only: \
             tool use, correctness, honesty about what failed, and code, commit messages and files you \
             write are unaffected. It is the user's explicit request, so it overrides the defaults above about \
             tone and emoji; keep answers as short as those rules ask.\n- {}",
            lines.join("\n- ")
        ))
    }

    /// A one-line summary for a settings row: "Friendly · warm+ · emoji−".
    pub fn summary(&self) -> String {
        let mut parts = vec![self.base.label().to_string()];
        for t in Trait::ALL {
            match self.level(t) {
                Level::More => parts.push(format!("{}+", t.label().to_lowercase())),
                Level::Less => parts.push(format!("{}−", t.label().to_lowercase())),
                Level::Default => {}
            }
        }
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_adds_nothing_to_the_prompt() {
        assert_eq!(Personality::default().prompt_section(), None);
    }

    #[test]
    fn a_choice_becomes_a_tone_only_section() {
        let p = Personality { base: BaseStyle::Friendly, warm: Level::More, emoji: Level::Less, ..Default::default() };
        let section = p.prompt_section().unwrap();
        assert!(section.contains("warm and chatty"));
        assert!(section.contains("warmer"));
        assert!(section.contains("Do not use emoji"));
        assert!(section.contains("tone and presentation only"));
        assert!(!section.contains("excitement"), "defaults add no line: {section}");
    }

    #[test]
    fn levels_and_styles_cycle_both_ways() {
        assert_eq!(Level::Default.cycle(true), Level::More);
        assert_eq!(Level::More.cycle(true), Level::Less);
        assert_eq!(Level::Less.cycle(false), Level::More);
        assert_eq!(BaseStyle::Default.cycle(false), BaseStyle::Cynical);
        assert_eq!(BaseStyle::Cynical.cycle(true), BaseStyle::Default);
    }

    #[test]
    fn it_round_trips_through_the_config_file() {
        let p = Personality { base: BaseStyle::Candid, headers_lists: Level::More, ..Default::default() };
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, r#"{"base":"candid","warm":"default","enthusiastic":"default","headers_lists":"more","emoji":"default"}"#);
        assert_eq!(serde_json::from_str::<Personality>(&json).unwrap(), p);
        assert_eq!(serde_json::from_str::<Personality>(r#"{"base":"quirky"}"#).unwrap().base, BaseStyle::Quirky);
    }
}
