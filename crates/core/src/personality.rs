//! Voice: a base style plus adjustable traits, tone only. It reaches the model
//! two ways: a system-prompt section written as who the assistant is (told
//! "the user chose a friendly tone", a model announces it), and
//! [`Personality::voice_prelude`], one earlier exchange in that voice, which
//! models follow more closely than any description. Behaviour is the same in
//! every style, and all-default adds nothing to the prompt.

use flashagent_llm::ChatMessage;

use serde::{Deserialize, Serialize};

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

    /// Second person.
    fn character(self) -> Option<&'static str> {
        Some(match self {
            Self::Default => return None,
            Self::Professional => {
                "You write like a senior engineer to a colleague they respect: measured, exact terms, no slang, no jokes."
            }
            Self::Friendly => {
                "You talk like a friendly colleague pairing at the same desk: warm, relaxed, glad to help, and still quick to the point."
            }
            Self::Candid => {
                "You are direct: you say plainly what is wrong or risky and what is good, and you encourage without flattering."
            }
            Self::Quirky => {
                "You have a playful streak: a light joke or a vivid comparison now and then, never at the cost of the answer."
            }
            Self::Efficient => "You use the fewest words that carry the answer: no greetings, no pleasantries, no recap.",
            Self::Cynical => {
                "You are dry and a little sarcastic about bad ideas and fragile code, never about the person, and the sarcasm never replaces a clear answer."
            }
        })
    }

    /// The answer to "what does `git stash` do?" in this style: opening and points.
    fn sample(self) -> (&'static str, [&'static str; 3]) {
        match self {
            Self::Default | Self::Professional => (
                "",
                [
                    "`git stash` records your uncommitted changes and restores a clean working tree.",
                    "`git stash pop` reapplies the most recent entry and removes it from the stash.",
                    "`git stash list` shows the saved entries.",
                ],
            ),
            Self::Friendly => (
                "Happy to help!",
                [
                    "`git stash` tucks your uncommitted changes away and gives you a clean working tree, handy when you need to switch branches mid-task.",
                    "When you're ready, `git stash pop` brings them right back.",
                    "And `git stash list` shows what you've got saved.",
                ],
            ),
            Self::Candid => (
                "",
                [
                    "`git stash` saves your uncommitted changes and cleans the working tree.",
                    "`git stash pop` restores them, but if the branch has moved on, expect conflicts; a short-lived branch is often the safer choice.",
                    "`git stash list` shows what is saved.",
                ],
            ),
            Self::Quirky => (
                "",
                [
                    "`git stash` is a coat check for half-finished work: it takes your uncommitted changes and hands you a clean working tree.",
                    "`git stash pop` is handing in the ticket.",
                    "`git stash list` shows every coat still on the rack.",
                ],
            ),
            Self::Efficient => (
                "",
                [
                    "Saves uncommitted changes and cleans the working tree.",
                    "`git stash pop` restores them.",
                    "`git stash list` lists them.",
                ],
            ),
            Self::Cynical => (
                "",
                [
                    "`git stash` hides your uncommitted changes and gives you a clean working tree: the place half-finished work goes to be forgotten.",
                    "`git stash pop` brings it back, conflicts included.",
                    "`git stash list` shows how much you have abandoned so far.",
                ],
            ),
        }
    }

    pub fn cycle(self, forward: bool) -> Self {
        let i = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        let n = Self::ALL.len();
        Self::ALL[if forward { (i + 1) % n } else { (i + n - 1) % n }]
    }
}

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

    pub fn cycle(self, forward: bool) -> Self {
        match (self, forward) {
            (Self::Less, true) | (Self::More, false) => Self::Default,
            (Self::Default, true) | (Self::Less, false) => Self::More,
            (Self::More, true) | (Self::Default, false) => Self::Less,
        }
    }
}

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
            Self::HeadersAndLists => "Headers and lists",
            Self::Emoji => "Emoji",
        }
    }

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

    fn character(self, level: Level) -> Option<&'static str> {
        Some(match (self, level) {
            (_, Level::Default) => return None,
            (Self::Warm, Level::More) => "You are warm and personable.",
            (Self::Warm, Level::Less) => "You keep warmth out of it: professional and factual.",
            (Self::Enthusiastic, Level::More) => "You bring energy; good news and neat solutions genuinely excite you.",
            (Self::Enthusiastic, Level::Less) => "You stay calm and even; nothing is exclaimed.",
            (Self::HeadersAndLists, Level::More) => "You lay answers out with a short header and lists where they help the reader.",
            (Self::HeadersAndLists, Level::Less) => "You write in flowing paragraphs rather than headers and bullet lists.",
            (Self::Emoji, Level::More) => "You use an emoji where it fits naturally.",
            (Self::Emoji, Level::Less) => "You never use emoji.",
        })
    }
}

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

    /// `None` when everything is default. No mention of a setting or a choice:
    /// a model told it was asked to sound some way says so in replies.
    pub fn prompt_section(&self) -> Option<String> {
        let lines: Vec<&str> = self
            .base
            .character()
            .into_iter()
            .chain(Trait::ALL.iter().filter_map(|t| t.character(self.level(*t))))
            .collect();
        if lines.is_empty() {
            return None;
        }
        Some(format!(
            "YOUR VOICE:\n{}\n\
             This is how you talk in replies, and nothing more: tools, code, commit messages, files and honesty \
             about what failed are the same whatever your voice. It is simply how you are, so never describe or \
             mention your own tone.",
            lines.join(" ")
        ))
    }

    pub fn uses_emoji(&self) -> bool {
        self.emoji == Level::More
    }

    /// Carried right after the system prompt. The question is unrelated to any
    /// project, so the model has nothing in it to refer back to.
    pub fn voice_prelude(&self) -> Vec<ChatMessage> {
        if *self == Personality::default() {
            return Vec::new();
        }
        vec![ChatMessage::user("Quick one: what does `git stash` do?"), ChatMessage::assistant(self.sample_answer())]
    }

    fn sample_answer(&self) -> String {
        let (opening, points) = self.base.sample();
        let mut opening = match (self.warm, opening.is_empty()) {
            (Level::Less, _) => String::new(),
            (Level::More, true) => "Good question.".to_string(),
            _ => opening.to_string(),
        };
        match self.enthusiastic {
            Level::More if opening.is_empty() => opening = "Oh, this one is handy!".to_string(),
            Level::More => opening = opening.replace('.', "!"),
            Level::Less => opening = opening.replace('!', "."),
            Level::Default => {}
        }
        if self.emoji == Level::More {
            opening = if opening.is_empty() { "📦".to_string() } else { format!("{opening} 📦") };
        }
        let as_list = match self.headers_lists {
            Level::More => true,
            Level::Less => false,
            Level::Default => matches!(self.base, BaseStyle::Default | BaseStyle::Professional | BaseStyle::Efficient),
        };
        let body = if as_list {
            let list = points.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n");
            if self.headers_lists == Level::More { format!("**git stash**\n\n{list}") } else { list }
        } else {
            points.join(" ")
        };
        if opening.is_empty() { body } else { format!("{opening}\n\n{body}") }
    }

    /// E.g. "Friendly · warm+ · emoji−".
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
    fn a_choice_becomes_who_the_assistant_is_not_a_setting() {
        let p = Personality { base: BaseStyle::Friendly, warm: Level::More, emoji: Level::Less, ..Default::default() };
        let section = p.prompt_section().unwrap();
        assert!(section.contains("friendly colleague"));
        assert!(section.contains("warm and personable"));
        assert!(section.contains("never use emoji"));
        assert!(!section.contains("excite"), "defaults add no line: {section}");
        // Words that make a model announce its instructions.
        for meta in ["user's choice", "requested", "setting", "Style and tone", "the user chose"] {
            assert!(!section.contains(meta), "{meta:?} in {section}");
        }
    }

    #[test]
    fn the_default_voice_adds_no_example() {
        assert!(Personality::default().voice_prelude().is_empty());
    }

    #[test]
    fn the_example_is_written_in_the_chosen_voice() {
        let answer = |p: Personality| p.voice_prelude()[1].content.clone();
        let quirky = answer(Personality { base: BaseStyle::Quirky, ..Default::default() });
        assert!(quirky.contains("coat check"), "{quirky}");
        assert!(!quirky.contains("- "), "a playful voice talks in sentences: {quirky}");

        let listed = answer(Personality { base: BaseStyle::Quirky, headers_lists: Level::More, ..Default::default() });
        assert!(listed.contains("**git stash**") && listed.contains("\n- "), "{listed}");

        let emoji = answer(Personality { base: BaseStyle::Efficient, emoji: Level::More, ..Default::default() });
        assert!(emoji.contains('📦'), "{emoji}");

        let calm = answer(Personality { base: BaseStyle::Friendly, enthusiastic: Level::Less, ..Default::default() });
        assert!(!calm.contains('!'), "{calm}");

        let cold = answer(Personality { base: BaseStyle::Friendly, warm: Level::Less, ..Default::default() });
        assert!(!cold.contains("Happy to help"), "{cold}");

        let prelude = Personality { base: BaseStyle::Candid, ..Default::default() }.voice_prelude();
        assert_eq!(prelude.len(), 2);
        assert_eq!(prelude[0].role, flashagent_llm::Role::User);
        assert_eq!(prelude[1].role, flashagent_llm::Role::Assistant);
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
