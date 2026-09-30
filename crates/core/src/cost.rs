//! What this session has cost, when the backend prices it at all.
//!
//! The number is only ever what the server said. Computing one from token
//! counts would need a price table that goes stale every time a provider
//! changes a rate, and would be quietly wrong for a model served at a
//! discount, through a subscription, or from a cache. OpenRouter sends
//! `usage.cost`; a local server sends nothing, and for a local server the
//! honest figure is zero rather than an invented one.

use std::collections::BTreeMap;

/// One backend's share of the bill.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderCost {
    /// The provider or endpoint the turns went through.
    pub name: String,
    pub total: f64,
    pub turns: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CostLedger {
    /// The order turns arrived, so `/cost` reads like a story.
    providers: BTreeMap<String, ProviderCost>,
    total: f64,
    /// Turns that actually came back with a price. Turns without one are not
    /// counted as free either: they are simply not known.
    priced_turns: u64,
}

impl CostLedger {
    /// What one turn cost. `None` from the server means unknown, and unknown is
    /// never added to the total: a running sum that silently skips what it
    /// could not price would be a number with a hole in it, and this is shown
    /// to the person as a real figure.
    pub fn record(&mut self, provider: &str, cost: Option<f64>) {
        let Some(cost) = cost.filter(|c| c.is_finite() && *c >= 0.0) else { return };
        let entry = self.providers.entry(provider.to_string()).or_insert_with(|| ProviderCost {
            name: provider.to_string(),
            total: 0.0,
            turns: 0,
        });
        entry.total += cost;
        entry.turns += 1;
        self.total += cost;
        self.priced_turns += 1;
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
    pub fn summary(&self) -> Option<String> {
        let total = self.total()?;
        Some(format!("${} · {}", Self::format_amount(total), Self::turns_phrase(self.priced_turns)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(ledger.summary().as_deref(), Some("$0.00 · 1 turn"), "a free turn is still a turn");
        ledger.record("p", Some(0.0));
        assert_eq!(ledger.summary().as_deref(), Some("$0.00 · 2 turns"));
    }
}
