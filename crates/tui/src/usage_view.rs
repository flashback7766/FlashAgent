//! `/usage`: what the session, the saved sessions and this install have spent,
//! and what the providers say is left.
//!
//! Four tabs over one dataset: the live session, the saved sessions on disk,
//! the lifetime file, and the providers' own limits. Every number says where
//! it came from; a provider that reports nothing is named as such rather than
//! shown as a confident zero.

use std::cell::Cell;
use std::collections::BTreeMap;

use flashagent_core::cost::UsageTotals;
use flashagent_core::ContextUsage;
use flashagent_llm::limits::LimitsOutcome;

use crate::i18n::tr;
use crate::{visible_width, LineKind, RenderLine};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageTab {
    Session,
    History,
    Lifetime,
    Limits,
}

impl UsageTab {
    const ALL: [UsageTab; 4] = [UsageTab::Session, UsageTab::History, UsageTab::Lifetime, UsageTab::Limits];

    fn label(self) -> &'static str {
        match self {
            Self::Session => "Session",
            Self::History => "History",
            Self::Lifetime => "Lifetime",
            Self::Limits => "Limits",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Session => Self::History,
            Self::History => Self::Lifetime,
            Self::Lifetime => Self::Limits,
            Self::Limits => Self::Session,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Session => Self::Limits,
            Self::History => Self::Session,
            Self::Lifetime => Self::History,
            Self::Limits => Self::Lifetime,
        }
    }
}

/// What the open session has spent, as the app knows it at the moment the menu
/// opens. The menu is a reading, not a live counter: the numbers move on with
/// the session behind it.
#[derive(Debug, Clone, Default)]
pub struct SessionUsage {
    pub provider: String,
    pub model: String,
    pub totals: UsageTotals,
    pub upstream: Option<f64>,
    pub allowance: Option<(f64, u64)>,
    pub priced_requests: u64,
    pub context: ContextUsage,
    pub turn_out: usize,
    pub turn_agents: usize,
}

/// One saved session's row. `usage` is what the file recorded; older files
/// recorded nothing, and their tokens are an estimate from the text with no
/// price, said as such.
#[derive(Debug, Clone, Default)]
pub struct HistoryRow {
    pub when: String,
    pub title: String,
    pub model: String,
    pub usage: Option<UsageTotals>,
    pub estimated: Option<UsageTotals>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageAction {
    None,
    Close,
    RefreshLimits,
}

pub struct UsageView {
    tab: UsageTab,
    scroll: Cell<usize>,
    /// The last render's body height in rows, so the keys know how far the
    /// scroll can go without guessing.
    body_rows: Cell<usize>,
    window: Cell<usize>,
    session: SessionUsage,
    history: Vec<HistoryRow>,
    lifetime: flashagent_core::usage_history::UsageHistory,
    /// One outcome per provider named in Settings, keyed by that name.
    limits: BTreeMap<String, LimitsOutcome>,
    pending_limits: Cell<usize>,
}

impl UsageView {
    pub fn new(
        session: SessionUsage,
        history: Vec<HistoryRow>,
        lifetime: flashagent_core::usage_history::UsageHistory,
        providers: Vec<String>,
        limits_loading: bool,
    ) -> Self {
        let mut limits = BTreeMap::new();
        for provider in providers {
            limits.insert(provider, LimitsOutcome::Unsupported("not checked yet"));
        }
        let pending = if limits_loading { limits.len() } else { 0 };
        Self {
            tab: UsageTab::Session,
            scroll: Cell::new(0),
            body_rows: Cell::new(0),
            window: Cell::new(10),
            session,
            history,
            lifetime,
            limits,
            pending_limits: Cell::new(pending),
        }
    }

    pub fn set_limits(&mut self, provider: String, outcome: LimitsOutcome) {
        self.limits.insert(provider, outcome);
        self.pending_limits.set(self.pending_limits.get().saturating_sub(1));
    }

    pub fn set_limits_loading(&mut self, loading: bool) {
        self.pending_limits.set(if loading { self.limits.len().max(1) } else { 0 });
    }

    pub fn slot(&self, provider: &str) -> Option<&LimitsOutcome> {
        self.limits.get(provider)
    }

    /// The providers whose limits the app should fetch.
    pub fn providers(&self) -> Vec<String> {
        self.limits.keys().cloned().collect()
    }

    pub fn handle_key(&mut self, code: crossterm::event::KeyCode, mods: crossterm::event::KeyModifiers) -> UsageAction {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => UsageAction::Close,
            KeyCode::Tab | KeyCode::Right => {
                self.switch(self.tab.next());
                UsageAction::None
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.switch(self.tab.prev());
                UsageAction::None
            }
            KeyCode::Char('r') | KeyCode::Char('R') => UsageAction::RefreshLimits,
            KeyCode::Down => {
                self.scroll_by(1);
                UsageAction::None
            }
            KeyCode::Up => {
                self.scroll_by(-1);
                UsageAction::None
            }
            KeyCode::PageDown => {
                let page = self.window.get().max(1);
                self.scroll_by(page as i32);
                UsageAction::None
            }
            KeyCode::PageUp => {
                let page = self.window.get().max(1);
                self.scroll_by(-(page as i32));
                UsageAction::None
            }
            KeyCode::Home => {
                self.scroll.set(0);
                UsageAction::None
            }
            KeyCode::End => {
                self.scroll.set(self.body_rows.get());
                UsageAction::None
            }
            _ => {
                let _ = mods;
                UsageAction::None
            }
        }
    }

    fn switch(&mut self, tab: UsageTab) {
        self.tab = tab;
        self.scroll.set(0);
    }

    fn scroll_by(&mut self, delta: i32) {
        let max = self.body_rows.get();
        let now = self.scroll.get() as i32 + delta;
        self.scroll.set(now.clamp(0, max as i32) as usize);
    }

    pub fn wheel(&mut self, dir: i32) -> bool {
        let before = self.scroll.get();
        self.scroll_by(dir * 3);
        self.scroll.get() != before
    }

    pub fn render(&self, width: usize, height: usize) -> Vec<RenderLine> {
        let border = "\x1b[38;2;95;90;85m";
        let reset = "\x1b[0m";
        let inner_w = width.saturating_sub(6).min(100);
        let mut lines = Vec::new();
        let title = format!(" {} ", tr("Usage"));
        let dashes = inner_w.saturating_sub(visible_width(&title) + 1);
        lines.push((LineKind::System, format!("  {border}╭─\x1b[1;38;2;225;175;95m{title}{border}{}╮{reset}", "─".repeat(dashes))));

        let pad_row = |content: &str| -> String {
            let clipped = crate::tool_views::clip_ellipsis(content, inner_w.saturating_sub(2));
            let pad = " ".repeat(inner_w.saturating_sub(visible_width(&clipped) + 2));
            format!("  {border}│{reset} {clipped}{pad} {border}│{reset}")
        };

        // The tab strip: the open one carries the accent.
        let mut strip = String::new();
        for (i, tab) in UsageTab::ALL.iter().enumerate() {
            if i > 0 {
                strip.push_str("  ");
            }
            if *tab == self.tab {
                strip.push_str(&format!("\x1b[1;38;2;225;175;95m[{}]\x1b[0m", tr(tab.label())));
            } else {
                strip.push_str(&format!("\x1b[38;2;160;155;145m{}\x1b[0m", tr(tab.label())));
            }
        }
        lines.push((LineKind::System, pad_row(&strip)));
        lines.push((LineKind::System, pad_row("")));

        let body = match self.tab {
            UsageTab::Session => self.session_rows(),
            UsageTab::History => self.history_rows(),
            UsageTab::Lifetime => self.lifetime_rows(),
            UsageTab::Limits => self.limit_rows(),
        };
        // Rows above the card's borders, the tab strip and two blanks, the
        // footer and the bottom border.
        // The card shares the screen with the tip and the status row; a card
        // taller than the room loses its tab strip off the top.
        let window = height.saturating_sub(11).max(4);
        self.window.set(window);
        let total = body.len();
        self.body_rows.set(total.saturating_sub(window));
        let start = self.scroll.get().min(total.saturating_sub(window));
        for row in body.iter().skip(start).take(window) {
            lines.push((LineKind::System, pad_row(row)));
        }
        // A body shorter than the window keeps the card from jumping.
        for _ in body.len().min(window)..window {
            lines.push((LineKind::System, pad_row("")));
        }

        let mut hint = String::from(&format!("\x1b[38;2;135;130;125m{}\x1b[0m", crate::key_hints(&[("Tab", "switch"), ("↑↓", "scroll"), ("Esc", "close")], inner_w.saturating_sub(2))));
        if self.tab == UsageTab::Limits {
            hint.push_str(&format!(" \x1b[38;2;135;130;125m· {}\x1b[0m", crate::key_hints(&[("r", "refresh")], inner_w.saturating_sub(2))));
        }
        lines.push((LineKind::System, pad_row(&hint)));
        if total > window {
            let from = start + 1;
            let to = (start + window).min(total);
            lines.push((LineKind::System, pad_row(&format!("\x1b[38;2;135;130;125m{from}-{to} / {total}\x1b[0m"))));
        } else {
            lines.push((LineKind::System, pad_row("")));
        }
        lines.push((LineKind::System, format!("  {border}╰{}╯{reset}", "─".repeat(inner_w))));
        lines
    }

    fn session_rows(&self) -> Vec<String> {
        let s = &self.session;
        let mut rows = Vec::new();
        let money = |v: f64| flashagent_core::CostLedger::format_amount(v);
        let tokens = |n: u64| ContextUsage::format_tokens(n as usize);
        rows.push(section(tr("Identity")));
        rows.push(kv(tr("Provider"), &s.provider));
        rows.push(kv(tr("Model"), &s.model));
        rows.push(String::new());
        rows.push(section(tr("Money")));
        let billed = if s.totals.priced_requests == 0 {
            tr("not reported by this API").to_string()
        } else if s.totals.cost == 0.0 {
            tr("free").to_string()
        } else {
            format!("${}", money(s.totals.cost))
        };
        rows.push(kv(tr("Billed"), &billed));
        if let Some(upstream) = s.upstream {
            rows.push(kv(tr("Upstream"), &format!("${}", money(upstream))));
        }
        if let Some((counted, turns)) = s.allowance {
            rows.push(kv(tr("Plan allowance"), &format!("${} · {}", money(counted), flashagent_core::CostLedger::turns_phrase(turns))));
        }
        rows.push(String::new());
        rows.push(section(tr("Tokens")));
        rows.push(kv(tr("Prompt"), &tokens(s.totals.prompt_tokens)));
        rows.push(kv(tr("Completion"), &tokens(s.totals.completion_tokens)));
        if s.totals.cached_tokens > 0 {
            let share = s.totals.cache_hit_percent().unwrap_or(0.0);
            rows.push(kv(tr("Cached"), &format!("{} · {share:.0}%", tokens(s.totals.cached_tokens))));
        }
        if s.totals.cache_written_tokens > 0 {
            rows.push(kv(tr("Cache written"), &tokens(s.totals.cache_written_tokens)));
        }
        if s.totals.reasoning_tokens > 0 {
            rows.push(kv(tr("Reasoning"), &tokens(s.totals.reasoning_tokens)));
        }
        rows.push(kv(tr("Requests"), &flashagent_core::CostLedger::turns_phrase(s.totals.requests)));
        rows.push(String::new());
        rows.push(section(tr("This turn")));
        rows.push(kv(tr("Out"), &tokens(s.turn_out as u64)));
        if s.turn_agents > 0 {
            rows.push(kv(tr("Agents"), &tokens(s.turn_agents as u64)));
        }
        rows.push(String::new());
        rows.push(section(tr("Context")));
        rows.push(kv(
            tr("Window"),
            &format!("{} / {} ({:.0}%)", tokens(s.context.total_used() as u64), tokens(s.context.total_capacity as u64), s.context.percentage()),
        ));
        for (label, value) in [
            (tr("System prompt"), s.context.system_tokens),
            (tr("Memory and project"), s.context.memory_tokens),
            (tr("Tool definitions"), s.context.tools_tokens),
            (tr("User prompts"), s.context.user_tokens),
            (tr("Assistant replies"), s.context.assistant_tokens),
            (tr("Reasoning content"), s.context.reasoning_tokens),
            (tr("Tool outputs"), s.context.tool_output_tokens),
        ] {
            if value > 0 {
                rows.push(kv(&format!("  {label}"), &tokens(value as u64)));
            }
        }
        if self.lifetime.since > 0 {
            rows.push(String::new());
            rows.push(section(tr("Lifetime")));
            rows.push(kv(tr("Requests"), &flashagent_core::CostLedger::turns_phrase(self.lifetime.totals.requests)));
            rows.push(kv(tr("Tokens"), &tokens(self.lifetime.totals.total_tokens())));
        }
        rows
    }

    fn history_rows(&self) -> Vec<String> {
        let mut rows = Vec::new();
        let recorded: Vec<&HistoryRow> = self.history.iter().filter(|r| r.usage.is_some()).collect();
        let estimated: Vec<&HistoryRow> = self.history.iter().filter(|r| r.usage.is_none()).collect();
        let mut tokens = 0u64;
        let mut cost = 0.0f64;
        for row in &recorded {
            if let Some(usage) = &row.usage {
                tokens = tokens.saturating_add(usage.total_tokens());
                cost += usage.cost;
            }
        }
        let mut estimated_tokens = 0u64;
        for row in &estimated {
            if let Some(usage) = &row.estimated {
                estimated_tokens = estimated_tokens.saturating_add(usage.total_tokens());
            }
        }
        rows.push(section(tr("Saved sessions")));
        rows.push(kv(tr("Sessions"), &flashagent_core::CostLedger::turns_phrase(self.history.len() as u64)));
        rows.push(kv(tr("With usage"), &flashagent_core::CostLedger::turns_phrase(recorded.len() as u64)));
        if !estimated.is_empty() {
            rows.push(kv(tr("Older, estimated"), &flashagent_core::CostLedger::turns_phrase(estimated.len() as u64)));
        }
        if tokens > 0 {
            rows.push(kv(tr("Tokens"), &ContextUsage::format_tokens(tokens as usize)));
        }
        if estimated_tokens > 0 {
            rows.push(kv(tr("Estimated tokens"), &ContextUsage::format_tokens(estimated_tokens as usize)));
        }
        if cost > 0.0 {
            rows.push(kv(tr("Cost"), &format!("${}", flashagent_core::CostLedger::format_amount(cost))));
        }
        if !recorded.is_empty() || !estimated.is_empty() {
            rows.push(String::new());
            rows.push(section(tr("Recent")));
        }
        for row in self.history.iter().take(30) {
            let title = if row.title.is_empty() { tr("(no prompt)") } else { &row.title };
            let usage = row.usage.as_ref().or(row.estimated.as_ref());
            let detail = match usage {
                Some(usage) => {
                    let mut parts = vec![format!("{} {}", ContextUsage::format_tokens(usage.total_tokens() as usize), tr("tokens"))];
                    if usage.priced_requests > 0 {
                        parts.push(format!("${}", flashagent_core::CostLedger::format_amount(usage.cost)));
                    } else if row.usage.is_none() {
                        parts.push(tr("cost not recorded").to_string());
                    }
                    parts.join(" · ")
                }
                None => tr("usage not recorded").to_string(),
            };
            rows.push(format!(
                "  \x1b[38;2;160;155;145m{}\x1b[0m \x1b[38;2;225;230;240m{}\x1b[0m",
                row.when,
                crate::tool_views::clip_ellipsis(title, 46)
            ));
            rows.push(format!(
                "      \x1b[38;2;135;130;125m{}\x1b[0m · \x1b[38;2;135;130;125m{detail}\x1b[0m",
                crate::tool_views::clip_ellipsis(&row.model, 32)
            ));
        }
        rows
    }

    fn lifetime_rows(&self) -> Vec<String> {
        let h = &self.lifetime;
        let mut rows = Vec::new();
        if h.totals.requests == 0 {
            rows.push(kv(tr("Lifetime"), tr("nothing recorded yet")));
            return rows;
        }
        let tokens = |n: u64| ContextUsage::format_tokens(n as usize);
        rows.push(section(tr("All runs")));
        rows.push(kv(tr("Since"), &flashagent_core::usage_history::day_key(h.since)));
        rows.push(kv(tr("Requests"), &flashagent_core::CostLedger::turns_phrase(h.totals.requests)));
        rows.push(kv(tr("Prompt"), &tokens(h.totals.prompt_tokens)));
        rows.push(kv(tr("Completion"), &tokens(h.totals.completion_tokens)));
        if h.totals.cached_tokens > 0 {
            rows.push(kv(tr("Cached"), &format!("{} · {:.0}%", tokens(h.totals.cached_tokens), h.totals.cache_hit_percent().unwrap_or(0.0))));
        }
        if h.totals.reasoning_tokens > 0 {
            rows.push(kv(tr("Reasoning"), &tokens(h.totals.reasoning_tokens)));
        }
        if h.totals.priced_requests > 0 {
            rows.push(kv(tr("Cost"), &format!("${}", flashagent_core::CostLedger::format_amount(h.totals.cost))));
        }
        rows.push(String::new());
        rows.push(section(tr("Last days")));
        for (day, totals) in h.recent_days(7) {
            rows.push(kv(&format!("  {day}"), &format!("{} tok · ${}", tokens(totals.total_tokens()), flashagent_core::CostLedger::format_amount(totals.cost))));
        }
        rows.push(String::new());
        rows.push(section(tr("By provider")));
        for (name, totals) in h.by_provider().into_iter().take(10) {
            rows.push(kv(&format!("  {name}"), &format!("{} tok · ${}", tokens(totals.total_tokens()), flashagent_core::CostLedger::format_amount(totals.cost))));
        }
        rows.push(String::new());
        rows.push(section(tr("By model")));
        for (name, totals) in h.by_model().into_iter().take(10) {
            rows.push(kv(&format!("  {name}"), &format!("{} tok · ${}", tokens(totals.total_tokens()), flashagent_core::CostLedger::format_amount(totals.cost))));
        }
        rows
    }

    fn limit_rows(&self) -> Vec<String> {
        let mut rows = Vec::new();
        if self.limits.is_empty() {
            rows.push(kv(tr("Providers"), tr("none configured")));
            return rows;
        }
        if self.pending_limits.get() > 0 {
            rows.push(format!("\x1b[38;2;225;175;95m{}\x1b[0m", tr("Asking the providers…")));
            rows.push(String::new());
        }
        for (provider, outcome) in &self.limits {
            rows.push(section(provider));
            match outcome {
                LimitsOutcome::Reported(lines) => {
                    for line in lines {
                        rows.push(kv(&format!("  {}", line.label), &line.value));
                    }
                }
                LimitsOutcome::Unsupported(why) => rows.push(format!("  \x1b[38;2;135;130;125m{why}\x1b[0m")),
                LimitsOutcome::Failed(why) => rows.push(format!("  \x1b[38;2;230;110;95m{why}\x1b[0m")),
            }
            rows.push(String::new());
        }
        rows
    }
}

fn section(text: &str) -> String {
    format!("\x1b[1;38;2;232;227;218m{text}\x1b[0m")
}

/// The labels are a column, so the values line up in either language.
fn kv(label: &str, value: &str) -> String {
    format!(
        "  \x1b[38;2;160;155;145m{label:<22}\x1b[0m \x1b[38;2;225;230;240m{value}\x1b[0m"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> UsageView {
        let session = SessionUsage {
            provider: "OpenCode Go".into(),
            model: "deepseek".into(),
            totals: UsageTotals { requests: 3, prompt_tokens: 1_000, completion_tokens: 200, cost: 0.25, priced_requests: 3, ..Default::default() },
            context: ContextUsage::new(1_000_000),
            turn_out: 120,
            ..Default::default()
        };
        UsageView::new(session, Vec::new(), Default::default(), vec!["OpenCode Go".into()], false)
    }

    #[test]
    fn the_session_tab_shows_what_the_ledger_recorded() {
        let lines = view().render(100, 40);
        let text = lines.iter().map(|(_, row)| crate::strip_ansi(row)).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Usage"));
        assert!(text.contains("OpenCode Go"));
        assert!(text.contains("$0.25"));
        assert!(text.contains("1k"));
    }

    #[test]
    fn tabs_switch_and_the_scroll_stays_inside_the_body() {
        let mut view = view();
        view.handle_key(crossterm::event::KeyCode::Tab, crossterm::event::KeyModifiers::NONE);
        assert_eq!(view.tab, UsageTab::History);
        view.handle_key(crossterm::event::KeyCode::Home, crossterm::event::KeyModifiers::NONE);
        view.handle_key(crossterm::event::KeyCode::Down, crossterm::event::KeyModifiers::NONE);
        assert!(view.scroll.get() <= view.body_rows.get());
    }
}
