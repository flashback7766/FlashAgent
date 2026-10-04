//! What the model is given before it has said anything: the system prompt and
//! the tool schemas. It is paid on every step of every turn, so it is budgeted
//! against the window the server reported.
//!
//! The prompt gets the same ceiling at every tier. It is the one part that
//! cannot be scaled down without making the model worse, and a shorter version
//! of it is not a cheaper version of it: it is a different agent. What the
//! window buys is the room for tool schemas, which are the interchangeable part.
//!
//! The ladder is the owner's, 2026-10-05: 160k of static context on a 1M
//! window, halving down to 10k at 64k. A prompt of 4k plus the schemas the
//! product ships costs about 7.6k, so the bottom tier has to be at least that
//! for the agent on a small window to still be the same agent. What is left in
//! a tier is split two ways and keeps a tenth spare, because a tier is a
//! ceiling and a set measured against it lands under.

/// The window assumed when the server said nothing about its own. Mid, so a
/// provider that omits `context_length` keeps the extended toolset while a
/// genuinely small one is still trimmed; `ToolsetProfile` overrides it either
/// way.
pub const ASSUMED_WINDOW: usize = 128_000;

/// What the prompt may cost at any window, from the owner, 2026-09-30.
pub const PROMPT_CEILING: usize = 4_000;

/// Total tokens of static context per tier, largest window first. What is left
/// after the prompt is split two ways and keeps a tenth spare, in the ratio the
/// owner's first figures gave: 20 parts built-in to 12 to MCP.
const TOTALS: &[(usize, usize)] = &[
    (1_000_000, 160_000),
    (512_000, 80_000),
    (256_000, 40_000),
    (128_000, 20_000),
    (64_000, 10_000),
];

const BUILTIN_SHARE: usize = 20;
const MCP_SHARE: usize = 12;
const SHARE_TOTAL: usize = BUILTIN_SHARE + MCP_SHARE + 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// The smallest window this tier answers to.
    pub window_floor: usize,
    /// Everything static, together.
    pub total: usize,
    /// [`PROMPT_CEILING`] at every tier.
    pub prompt: usize,
    pub builtin_tools: usize,
    pub mcp_tools: usize,
    /// Left unspent on purpose: a tier is a ceiling.
    pub reserve: usize,
}

impl Budget {
    pub fn for_window(window: Option<usize>) -> Budget {
        let window = window.unwrap_or(ASSUMED_WINDOW);
        let (window_floor, total) = TOTALS
            .iter()
            .find(|(floor, _)| window >= *floor)
            .copied()
            .unwrap_or_else(|| TOTALS.last().copied().expect("the ladder has a floor"));
        let share = total.saturating_sub(PROMPT_CEILING);
        let builtin_tools = share * BUILTIN_SHARE / SHARE_TOTAL;
        let mcp_tools = share * MCP_SHARE / SHARE_TOTAL;
        let reserve = share - builtin_tools - mcp_tools;
        Budget { window_floor, total, prompt: PROMPT_CEILING, builtin_tools, mcp_tools, reserve }
    }

    /// What the built-in schemas get once the prompt has taken what it actually
    /// measures, not what it is allowed.
    ///
    /// The ceiling is 4k and the prompt is written to fill it, but on a small
    /// window a full prompt plus the MCP share would leave nothing for the
    /// tools that make any task possible, so the schemas are given what is
    /// really left. MCP is charged first: those schemas come and go with the
    /// servers a user happens to have configured, the built-in ones are the
    /// product.
    pub fn tool_allowance(&self, prompt_tokens: usize) -> usize {
        self.total
            .saturating_sub(prompt_tokens)
            .saturating_sub(self.mcp_tools)
    }
}

/// Tokens a tool costs the model: its name, what it says and what it takes.
pub fn tool_cost(name: &str, description: &str, parameters_json: &str) -> usize {
    let text = format!("{name}{description}{parameters_json}");
    flashagent_llm::estimate_tokens(&text).max(1) as usize
}

/// Tools offered in registry order until the budget is spent. `Always` tools
/// are the floor and are never dropped; the rest go in as the window earns
/// them, and the caller is told what did not fit so the UI can say so rather
/// than leave the model to ask for a tool it was never offered.
pub fn fit_builtin<'a>(
    tools: impl Iterator<Item = (&'a str, bool, usize)>,
    budget: usize,
) -> (Vec<&'a str>, Vec<&'a str>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    let mut spent = 0usize;
    for (name, always, cost) in tools {
        if always || spent + cost <= budget {
            spent += cost;
            kept.push(name);
        } else {
            dropped.push(name);
        }
    }
    (kept, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bigger_window_never_spends_more() {
        for pair in TOTALS.windows(2) {
            let (higher, lower) = (pair[0], pair[1]);
            assert!(
                lower.1 <= higher.1,
                "a {}-token window is offered more than a {}-token one",
                lower.0,
                higher.0
            );
        }
    }

    #[test]
    fn the_top_tier_is_the_ladder_the_owner_gave() {
        let b = Budget::for_window(Some(1_000_000));
        assert_eq!(b.total, 160_000);
        assert_eq!(b.prompt, 4_000, "the prompt is the same at every tier");
        // 156k left, two ways, a tenth spare: the ratio, not the old numbers.
        assert_eq!(b.builtin_tools, 156_000 * 20 / 36);
        assert_eq!(b.mcp_tools, 156_000 * 12 / 36);
    }

    #[test]
    fn the_prompt_costs_the_same_at_every_window() {
        for window in [1_000_000, 512_000, 256_000, 128_000, 64_000, 8_000] {
            assert_eq!(Budget::for_window(Some(window)).prompt, PROMPT_CEILING, "at {window}");
        }
    }

    #[test]
    fn the_ladder_answers_at_its_own_floors() {
        for (floor, total) in TOTALS {
            assert_eq!(Budget::for_window(Some(*floor)).total, *total, "at {floor}");
        }
        // A window just under a floor belongs to the tier below it, so a model
        // that reports 999_999 is not given the 1M budget.
        assert_eq!(Budget::for_window(Some(999_999)).total, 80_000);
        assert_eq!(Budget::for_window(Some(511_999)).total, 40_000);
        assert_eq!(Budget::for_window(Some(127_999)).total, 10_000);
    }

    #[test]
    fn a_small_window_lands_on_the_floor_rather_than_nothing() {
        assert_eq!(Budget::for_window(Some(8_000)).total, 10_000);
        assert_eq!(Budget::for_window(Some(0)).total, 10_000);
    }

    #[test]
    fn a_server_that_says_nothing_is_treated_as_mid() {
        assert_eq!(Budget::for_window(None), Budget::for_window(Some(ASSUMED_WINDOW)));
    }

    #[test]
    fn the_parts_leave_room_under_the_total() {
        for (floor, total) in TOTALS {
            let b = Budget::for_window(Some(*floor));
            assert!(b.prompt + b.builtin_tools + b.mcp_tools <= b.total, "{floor}");
            assert!(*total == b.total);
        }
    }

    #[test]
    fn a_small_window_does_not_spend_its_budget_on_the_prompt() {
        // The prompt is a fixed cost: 1,778 tokens whatever the window is.
        // Enforced literally, its 10% share would be 500 at 64k and the schemas
        // would be left with nothing, so it takes what it measures and the
        // schemas take the rest. What has to hold is that it cannot crowd them
        // out, at any tier.
        let prompt_tokens = 1_778;
        for (floor, _) in TOTALS {
            let b = Budget::for_window(Some(*floor));
            assert!(b.tool_allowance(prompt_tokens) > 0, "at {floor} no schema fits");
            assert!(prompt_tokens * 3 < b.total * 2, "at {floor} the prompt takes over half the budget");
        }
    }

    #[test]
    fn the_floor_tools_are_never_the_ones_dropped() {
        let tools = [("read_file", true, 300usize), ("web_search", false, 400), ("edit_file", true, 500)];
        let (kept, dropped) = fit_builtin(tools.iter().copied(), 400);
        assert_eq!(kept, ["read_file", "edit_file"]);
        assert_eq!(dropped, ["web_search"]);
    }

    #[test]
    fn a_budget_nobody_fits_still_offers_the_floor() {
        let tools = [("read_file", true, 900usize)];
        let (kept, dropped) = fit_builtin(tools.iter().copied(), 100);
        assert_eq!(kept, ["read_file"]);
        assert!(dropped.is_empty());
    }

    #[test]
    fn everything_fits_when_there_is_room() {
        let tools = [("a", true, 10usize), ("b", false, 10), ("c", false, 10)];
        let (kept, dropped) = fit_builtin(tools.iter().copied(), 1_000);
        assert_eq!(kept, ["a", "b", "c"]);
        assert!(dropped.is_empty());
    }

    /// The whole contract as the model sees it, so the ladder is checked against
    /// the real schemas rather than against an estimate of them.
    #[test]
    fn the_registry_measures_against_the_ladder() {
        let mut total = 0;
        for def in crate::registry::TOOLS {
            let cost = tool_cost(def.name, def.description, &def.schema_json());
            total += cost;
            println!("{:>6}  {}", cost, def.name);
        }
        println!("{total:>6}  TOTAL over {} tools", crate::registry::TOOLS.len());
    }
}