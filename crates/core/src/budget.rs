//! What the model is given before it has said anything: the system prompt and
//! the tool schemas. It is paid on every step of every turn, so it is budgeted
//! against the window the server reported.
//!
//! The prompt gets the same ceiling at every window. It is the one part that
//! cannot be scaled down without making the model worse, and a shorter version
//! of it is not a cheaper version of it: it is a different agent.
//!
//! The static budget is flat for every model: the prompt (up to 10k) plus the
//! full toolset (~5k) = 20k, regardless of the window size. There is no ladder
//! anymore; every window gets the same 20k.

/// The window assumed when the server said nothing about its own. It no longer
/// changes the static total, which is flat for every window; `ToolsetProfile`
/// still overrides the toolset explicitly.
pub const ASSUMED_WINDOW: usize = 128_000;

/// What the prompt may cost at any window, from the owner, 2026-10-05.
///
/// A full contract runs to about this, and it is the same at every window: a
/// shorter one is not a cheaper agent, it is a different one. The rest of the
/// budget is for schemas.
pub const PROMPT_CEILING: usize = 10_000;

/// Total tokens of static context, the same for every window. What is left
/// after the prompt is split two ways and keeps a tenth spare, in the ratio the
/// owner's first figures gave: 20 parts built-in to 12 to MCP.
const TOTALS: &[(usize, usize)] = &[(0, 20_000)];

const BUILTIN_SHARE: usize = 20;
const MCP_SHARE: usize = 12;
const SHARE_TOTAL: usize = BUILTIN_SHARE + MCP_SHARE + 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// The smallest window this tier answers to.
    pub window_floor: usize,
    /// Everything static, together.
    pub total: usize,
    /// [`PROMPT_CEILING`] at every window.
    pub prompt: usize,
    pub builtin_tools: usize,
    pub mcp_tools: usize,
    /// Left unspent on purpose: the total is a ceiling.
    pub reserve: usize,
}

impl Budget {
    pub fn for_window(window: Option<usize>) -> Budget {
        let window = window.unwrap_or(ASSUMED_WINDOW);
        let (window_floor, total) = TOTALS
            .iter()
            .find(|(floor, _)| window >= *floor)
            .copied()
            .unwrap_or_else(|| TOTALS.last().copied().expect("the flat budget has a floor"));
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

/// Tokens a tool costs the model: its name, what it says and what it takes,
/// counted with the tokenizer of the model it will be sent to. A budget decided
/// on another model's tokenizer is a budget for another model.
pub fn tool_cost(model: &str, name: &str, description: &str, parameters_json: &str) -> usize {
    let text = format!("{name}{description}{parameters_json}");
    flashagent_llm::count_tokens(model, &text).max(1)
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
    fn the_flat_budget_splits_what_the_prompt_leaves() {
        let b = Budget::for_window(Some(1_000_000));
        assert_eq!(b.total, 20_000);
        assert_eq!(b.prompt, PROMPT_CEILING, "the prompt is the same at every window");
        assert_eq!(b.builtin_tools, (20_000 - PROMPT_CEILING) * 20 / 36);
        assert_eq!(b.mcp_tools, (20_000 - PROMPT_CEILING) * 12 / 36);
    }

    #[test]
    fn the_prompt_costs_the_same_at_every_window() {
        for window in [1_000_000, 512_000, 256_000, 128_000, 64_000, 8_000] {
            assert_eq!(Budget::for_window(Some(window)).prompt, PROMPT_CEILING, "at {window}");
        }
    }

    #[test]
    fn the_flat_budget_applies_to_every_window() {
        for window in [1_000_000, 512_000, 256_000, 128_000, 64_000, 8_000, 0] {
            assert_eq!(Budget::for_window(Some(window)).total, 20_000, "at {window}");
        }
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

    /// The whole contract as the model sees it, so the flat budget is checked
    /// against the real schemas rather than against an estimate of them.
    #[test]
    fn the_registry_measures_against_the_flat_budget() {
        let mut total = 0;
        for def in crate::registry::TOOLS {
            let cost = tool_cost("gpt-4o", def.name, def.description, &def.schema_json());
            total += cost;
            println!("{:>6}  {}", cost, def.name);
        }
        println!("{total:>6}  TOTAL over {} tools", crate::registry::TOOLS.len());
    }
}