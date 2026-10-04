//! What the model is given before it has said anything: the system prompt and
//! the tool schemas. It is paid on every step of every turn, so it is budgeted
//! against the window the server reported.
//!
//! A bigger window carries a richer contract and a small one a shorter
//! contract, but neither loses the tools that make a task possible at all. The
//! split inside a tier is the owner's, measured rather than guessed: on a 1M
//! window the prompt gets 4k tokens, the built-in tools 20k and the schemas a
//! connected MCP server adds 12k, which is 40k in total.

/// The window assumed when the server said nothing about its own. Mid, so a
/// provider that omits `context_length` keeps the extended toolset while a
/// genuinely small one is still trimmed; `ToolsetProfile` overrides it either
/// way.
pub const ASSUMED_WINDOW: usize = 128_000;

/// Total tokens of static context per tier, largest window first.
const TOTALS: &[(usize, usize)] = &[
    (1_000_000, 40_000),
    (512_000, 30_000),
    (256_000, 20_000),
    (128_000, 10_000),
    (64_000, 5_000),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// The smallest window this tier answers to.
    pub window_floor: usize,
    /// Everything static, together.
    pub total: usize,
    pub prompt: usize,
    pub builtin_tools: usize,
    pub mcp_tools: usize,
}

/// The owner's split of a total: a tenth for the prompt, half for the built-in
/// tools, three tenths for what MCP servers add. Ten per cent is left over on
/// purpose, because a tier is a ceiling and a schema set measured against it
/// lands just under it.
const fn split(total: usize) -> (usize, usize, usize) {
    (total / 10, total / 2, total * 3 / 10)
}

impl Budget {
    pub fn for_window(window: Option<usize>) -> Budget {
        let window = window.unwrap_or(ASSUMED_WINDOW);
        let (window_floor, total) = TOTALS
            .iter()
            .find(|(floor, _)| window >= *floor)
            .copied()
            .unwrap_or_else(|| TOTALS.last().copied().expect("the ladder has a floor"));
        let (prompt, builtin_tools, mcp_tools) = split(total);
        Budget { window_floor, total, prompt, builtin_tools, mcp_tools }
    }

    /// The tools every task needs: read, write, search, shell, the question
    /// tool. They are offered whatever the budget says, so a tier can only make
    /// the toolset smaller around them and never hollow it out.
    pub fn floor_is_always(&self) -> bool {
        true
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
    fn the_top_tier_is_the_split_the_owner_gave() {
        let b = Budget::for_window(Some(1_000_000));
        assert_eq!((b.total, b.prompt, b.builtin_tools, b.mcp_tools), (40_000, 4_000, 20_000, 12_000));
    }

    #[test]
    fn the_ladder_answers_at_its_own_floors() {
        for (floor, total) in TOTALS {
            assert_eq!(Budget::for_window(Some(*floor)).total, *total, "at {floor}");
        }
        // A window just under a floor belongs to the tier below it, so a model
        // that reports 999_999 is not given the 1M budget.
        assert_eq!(Budget::for_window(Some(999_999)).total, 30_000);
        assert_eq!(Budget::for_window(Some(511_999)).total, 20_000);
        assert_eq!(Budget::for_window(Some(127_999)).total, 5_000);
    }

    #[test]
    fn a_small_window_lands_on_the_floor_rather_than_nothing() {
        assert_eq!(Budget::for_window(Some(8_000)).total, 5_000);
        assert_eq!(Budget::for_window(Some(0)).total, 5_000);
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