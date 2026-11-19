//! What this session has cost, when the backend prices it at all.
//!
//! The number is only ever what the server said. Computing one from token
//! counts would need a price table that goes stale every time a provider
//! changes a rate, and would be quietly wrong for a model served at a
//! discount, through a subscription, or from a cache. OpenRouter sends
//! `usage.cost`; a local server sends nothing, and for a local server the
//! honest figure is zero rather than an invented one.

use std::collections::BTreeMap;

/// Token and money totals over any set of requests. The same shape is used for
/// the session, one provider and one model, so the view can add them up
/// without a second set of field names.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct UsageTotals {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cache_written_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost: f64,
    pub upstream_cost: f64,
    pub allowance: f64,
    pub priced_requests: u64,
    pub allowance_requests: u64,
}

impl UsageTotals {
    pub fn add_usage(&mut self, usage: &flashagent_llm::Usage) {
        self.requests = self.requests.saturating_add(1);
        self.prompt_tokens = self.prompt_tokens.saturating_add(usage.prompt.unwrap_or(0).max(0) as u64);
        self.completion_tokens = self.completion_tokens.saturating_add(usage.completion.unwrap_or(0).max(0) as u64);
        self.cached_tokens = self.cached_tokens.saturating_add(usage.cached.unwrap_or(0).max(0) as u64);
        self.cache_written_tokens =
            self.cache_written_tokens.saturating_add(usage.cache_written.unwrap_or(0).max(0) as u64);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(usage.reasoning.unwrap_or(0).max(0) as u64);
        if let Some(cost) = usage.cost.filter(|c| c.is_finite() && *c >= 0.0) {
            self.cost += cost;
            self.priced_requests = self.priced_requests.saturating_add(1);
        }
        if let Some(upstream) = usage.cost_details.and_then(|d| d.upstream_cost).filter(|c| c.is_finite() && *c >= 0.0) {
            self.upstream_cost += upstream;
        }
        if let Some(counted) = usage.allowance.filter(|c| c.is_finite() && *c >= 0.0) {
            self.allowance += counted;
            self.allowance_requests = self.allowance_requests.saturating_add(1);
        }
    }

    pub fn merge(&mut self, other: &Self) {
        self.requests = self.requests.saturating_add(other.requests);
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self.completion_tokens.saturating_add(other.completion_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(other.cached_tokens);
        self.cache_written_tokens = self.cache_written_tokens.saturating_add(other.cache_written_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(other.reasoning_tokens);
        self.cost += other.cost;
        self.upstream_cost += other.upstream_cost;
        self.allowance += other.allowance;
        self.priced_requests = self.priced_requests.saturating_add(other.priced_requests);
        self.allowance_requests = self.allowance_requests.saturating_add(other.allowance_requests);
    }

    /// The share of the prompt that came from the cache, when the API said
    /// enough to know it.
    pub fn cache_hit_percent(&self) -> Option<f64> {
        let counted = self.cached_tokens.saturating_add(self.cache_written_tokens);
        (counted > 0).then(|| 100.0 * self.cached_tokens as f64 / self.prompt_tokens.max(1) as f64)
    }

    /// The token figure that actually says how much text went through: the
    /// cached prefix is part of the prompt and is not added again.
    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens.saturating_add(self.completion_tokens)
    }
}

/// One backend's share of the bill.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderCost {
    /// The provider or endpoint the turns went through.
    pub name: String,
    pub total: f64,
    pub turns: u64,
    pub usage: UsageTotals,
}

/// One provider-and-model pair, for the view's model rows.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCost {
    pub provider: String,
    pub model: String,
    pub usage: UsageTotals,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CostLedger {
    /// The order turns arrived, so `/cost` reads like a story.
    providers: BTreeMap<String, ProviderCost>,
    models: BTreeMap<(String, String), UsageTotals>,
    usage: UsageTotals,
    total: f64,
    /// Turns that actually came back with a price. Turns without one are not
    /// counted as free either: they are simply not known.
    priced_turns: u64,
    /// What the providers behind the gateways charged, when they said. The
    /// difference from the total is what the gateway added for itself.
    upstream_total: f64,
    upstream_turns: u64,
    /// What a plan counted against its monthly allowance, when the account
    /// bills by quota rather than per turn: OpenCode's gateway sends this on
    /// every response and no per-turn price, so it is the only figure there.
    allowance_total: f64,
    allowance_turns: u64,
}

impl CostLedger {
    /// What one turn cost. `None` from the server means unknown, and unknown is
    /// never added to the total: a running sum that silently skips what it
    /// could not price would be a number with a hole in it, and this is shown
    /// to the person as a real figure.
    pub fn record(&mut self, provider: &str, cost: Option<f64>) {
        self.record_with_details(provider, cost, None);
    }

    /// What the turn cost, and what the provider behind the gateway said it
    /// cost. The second number is what the upstream provider charged: the
    /// difference from the first is the gateway's own margin.
    pub fn record_with_details(&mut self, provider: &str, cost: Option<f64>, upstream: Option<f64>) {
        if let Some(upstream) = upstream.filter(|c| c.is_finite() && *c >= 0.0) {
            self.upstream_total += upstream;
            self.upstream_turns += 1;
        }
        let Some(cost) = cost.filter(|c| c.is_finite() && *c >= 0.0) else { return };
        let entry = self.providers.entry(provider.to_string()).or_insert_with(|| ProviderCost {
            name: provider.to_string(),
            total: 0.0,
            turns: 0,
            usage: UsageTotals::default(),
        });
        entry.total += cost;
        entry.turns += 1;
        self.total += cost;
        self.priced_turns += 1;
    }

    /// What the providers behind the gateways charged, when they said.
    pub fn upstream_total(&self) -> Option<f64> {
        (self.upstream_turns > 0).then_some(self.upstream_total)
    }

    /// A request's tokens and its share of the bill and of any plan's
    /// allowance, from the usage the server sent. The main loop and a child's
    /// turns both come through here: a child's turn is the same account's.
    pub fn record_usage(&mut self, provider: &str, model: &str, usage: &flashagent_llm::Usage) {
        self.record_with_details(provider, usage.cost, usage.cost_details.and_then(|d| d.upstream_cost));
        self.record_allowance(usage.allowance);
        self.usage.add_usage(usage);
        let entry = self.providers.entry(provider.to_string()).or_insert_with(|| ProviderCost {
            name: provider.to_string(),
            total: 0.0,
            turns: 0,
            usage: UsageTotals::default(),
        });
        entry.usage.add_usage(usage);
        self.models.entry((provider.to_string(), model.to_string())).or_default().add_usage(usage);
    }

    /// The session's tokens and money, in one shape.
    pub fn totals(&self) -> &UsageTotals {
        &self.usage
    }

    /// Every provider-and-model pair, most spent first.
    pub fn by_model(&self) -> Vec<ModelCost> {
        let mut all: Vec<ModelCost> = self
            .models
            .iter()
            .map(|((provider, model), usage)| ModelCost { provider: provider.clone(), model: model.clone(), usage: usage.clone() })
            .collect();
        all.sort_by(|a, b| {
            b.usage
                .cost
                .partial_cmp(&a.usage.cost)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.usage.total_tokens().cmp(&a.usage.total_tokens()))
                .then_with(|| a.model.cmp(&b.model))
        });
        all
    }

    /// A turn counted against a plan's monthly allowance: the provider's own
    /// prices, spent from a quota instead of billed.
    pub fn record_allowance(&mut self, counted: Option<f64>) {
        let Some(counted) = counted.filter(|c| c.is_finite() && *c >= 0.0) else { return };
        self.allowance_total += counted;
        self.allowance_turns += 1;
    }

    /// `None` until a plan counted something: a gateway that bills per turn
    /// sends nothing here, and a zero would read as an unused plan.
    pub fn allowance(&self) -> Option<(f64, u64)> {
        (self.allowance_turns > 0).then_some((self.allowance_total, self.allowance_turns))
    }

    pub fn upstream_turns(&self) -> u64 {
        self.upstream_turns
    }

    /// `None` until something has actually been priced, so the status bar can
    /// stay silent on a local model instead of showing a confident "$0.00".
    pub fn total(&self) -> Option<f64> {
        (self.priced_turns > 0).then_some(self.total)
    }

    pub fn priced_turns(&self) -> u64 {
        self.priced_turns
    }

    /// Every provider that was used, most spent first.
    pub fn by_provider(&self) -> Vec<ProviderCost> {
        let mut all: Vec<ProviderCost> = self.providers.values().cloned().collect();
        all.sort_by(|a, b| b.total.partial_cmp(&a.total).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.name.cmp(&b.name)));
        all
    }
}

impl CostLedger {
    /// A bill small enough that four decimals say more than two.
    pub fn format_amount(amount: f64) -> String {
        if amount > 0.0 && amount < 0.01 {
            format!("{amount:.4}")
        } else {
            format!("{amount:.2}")
        }
    }

    /// `1 turn` or `7 turns`, the way a person would say it.
    pub fn turns_phrase(turns: u64) -> String {
        if turns == 1 {
            "1 turn".to_string()
        } else {
            format!("{turns} turns")
        }
    }

    /// The status-bar line, or `None` when nothing has been priced.
    ///
    /// A total of zero is said as `free` and not as `$0.00`: the server priced
    /// every turn at nothing, which is a fact about the model, and writing it as
    /// an amount with a turn count beside it reads as a bill that came out empty
    /// rather than as one that does not exist. The turn count goes with the
    /// money, not with the word.
    pub fn summary(&self) -> Option<String> {
        let total = self.total()?;
        if total == 0.0 {
            return Some("free".to_string());
        }
        Some(format!("${} · {}", Self::format_amount(total), Self::turns_phrase(self.priced_turns)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_allowance_is_kept_apart_from_the_bill() {
        let mut ledger = CostLedger::default();
        ledger.record_allowance(None);
        assert_eq!(ledger.allowance(), None, "nothing counted is not zero counted");
        ledger.record_allowance(Some(0.0042));
        ledger.record_allowance(Some(0.0001));
        ledger.record_allowance(Some(-3.0));
        let (counted, turns) = ledger.allowance().unwrap();
        assert!((counted - 0.0043).abs() < 1e-9);
        assert_eq!(turns, 2, "a negative count is not a turn");
        assert_eq!(ledger.total(), None, "a quota is not a bill");
    }

    #[test]
    fn a_turns_usage_carries_its_bill_and_its_quota_together() {
        let mut ledger = CostLedger::default();
        let usage = flashagent_llm::Usage {
            cost: Some(0.002),
            cost_details: Some(flashagent_llm::types::CostDetails { upstream_cost: Some(0.0015) }),
            allowance: Some(0.0042),
            ..Default::default()
        };
        ledger.record_usage("OpenCode Go", "deepseek", &usage);
        assert_eq!(ledger.total(), Some(0.002));
        assert_eq!(ledger.upstream_total(), Some(0.0015));
        assert_eq!(ledger.allowance(), Some((0.0042, 1)), "a child's turn is the same account's");
        // A turn that says nothing adds nothing.
        ledger.record_usage("OpenCode Go", "deepseek", &flashagent_llm::Usage::default());
        assert_eq!(ledger.priced_turns(), 1, "a turn that prices nothing is not priced");
        assert_eq!(ledger.total(), Some(0.002));
    }

    #[test]
    fn tokens_are_kept_beside_the_bill_by_provider_and_model() {
        let mut ledger = CostLedger::default();
        let usage = flashagent_llm::Usage {
            prompt: Some(1_000),
            completion: Some(200),
            cached: Some(900),
            reasoning: Some(50),
            cost: Some(0.002),
            ..Default::default()
        };
        ledger.record_usage("OpenRouter", "alpha", &usage);
        // The same model again, with a colder prompt and no price.
        ledger.record_usage(
            "OpenRouter",
            "alpha",
            &flashagent_llm::Usage { prompt: Some(500), completion: Some(100), cached: Some(100), ..Default::default() },
        );
        let totals = ledger.totals();
        assert_eq!(
            (totals.requests, totals.prompt_tokens, totals.completion_tokens, totals.cached_tokens, totals.reasoning_tokens),
            (2, 1_500, 300, 1_000, 50)
        );
        assert_eq!(totals.priced_requests, 1, "a request the server did not price is not priced");
        assert!((totals.cache_hit_percent().unwrap() - 66.666).abs() < 0.01, "one hit share over both prompts");
        assert_eq!(ledger.by_provider()[0].usage.requests, 2, "the provider keeps its own tokens");
        let models = ledger.by_model();
        assert_eq!(models.len(), 1);
        assert_eq!((models[0].provider.as_str(), models[0].model.as_str()), ("OpenRouter", "alpha"));
        assert_eq!(models[0].usage.total_tokens(), 1_800);
    }

    #[test]
    fn a_backend_that_prices_nothing_leaves_the_total_unknown() {
        let mut ledger = CostLedger::default();
        assert_eq!(ledger.total(), None, "unknown is not zero");
        for _ in 0..5 {
            ledger.record("local", None);
        }
        assert_eq!(ledger.total(), None, "a local server is not a free server, it is an unpriced one");
        assert_eq!(ledger.summary(), None, "so the bar says nothing rather than $0.00");
    }

    #[test]
    fn a_priced_turn_is_summed_across_turns_and_providers() {
        let mut ledger = CostLedger::default();
        ledger.record("openrouter", Some(0.002));
        ledger.record("openrouter", Some(0.003));
        ledger.record("anthropic", Some(0.010));
        assert!((ledger.total().unwrap() - 0.015).abs() < 1e-9);
        assert_eq!(ledger.priced_turns(), 3);
    }

    #[test]
    fn an_absurd_price_is_refused_rather_than_shown() {
        let mut ledger = CostLedger::default();
        ledger.record("x", Some(f64::NAN));
        ledger.record("x", Some(f64::INFINITY));
        ledger.record("x", Some(-1.0));
        assert_eq!(ledger.total(), None, "a broken number must not reach the bill");
    }

    #[test]
    fn the_breakdown_leads_with_where_the_money_went() {
        let mut ledger = CostLedger::default();
        ledger.record("local", None);
        ledger.record("cheap", Some(0.001));
        ledger.record("dear", Some(0.050));
        let rows = ledger.by_provider();
        assert_eq!(rows[0].name, "dear");
        assert_eq!(rows[1].name, "cheap");
        assert!(rows.iter().all(|r| r.turns > 0), "an unpriced endpoint is not listed as a spent one");
    }

    #[test]
    fn a_small_bill_is_shown_precisely_enough_to_be_read() {
        assert_eq!(CostLedger::format_amount(0.00042), "0.0004");
        assert_eq!(CostLedger::format_amount(0.5), "0.50");
        assert_eq!(CostLedger::format_amount(12.0), "12.00");
    }

    #[test]
    fn the_summary_counts_turns_the_way_a_person_would_say_them() {
        assert_eq!(CostLedger::turns_phrase(1), "1 turn");
        assert_eq!(CostLedger::turns_phrase(0), "0 turns");
        assert_eq!(CostLedger::turns_phrase(7), "7 turns");
        let mut ledger = CostLedger::default();
        assert_eq!(ledger.summary(), None);
        ledger.record("p", Some(0.0));
        assert_eq!(ledger.summary().as_deref(), Some("free"), "a model the server prices at nothing is free, and saying so beats writing $0.00");
        ledger.record("p", Some(0.0));
        assert_eq!(ledger.summary().as_deref(), Some("free"), "and it stays that way however many turns go by");
        ledger.record("p", Some(0.25));
        assert_eq!(ledger.summary().as_deref(), Some("$0.25 · 3 turns"), "once there is money, the bill is back");
    }

    #[test]
    fn a_free_model_and_an_unpriced_one_are_not_confused() {
        let mut free = CostLedger::default();
        free.record("p", Some(0.0));
        assert_eq!(free.summary().as_deref(), Some("free"), "the server said zero, so the model is free");
        let mut local = CostLedger::default();
        local.record("p", None);
        assert_eq!(local.summary(), None, "a server that says nothing is not free, it is unknown");
    }
}
