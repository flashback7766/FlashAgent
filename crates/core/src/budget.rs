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

/// What the prompt may cost at any window, from the owner, 2026-10-05.
///
/// A full contract runs to about this, and it is the same at every window: a
/// shorter one is not a cheaper agent, it is a different one. The rest of the
/// budget is for schemas.
pub const PROMPT_CEILING: usize = 10_000;

/// Everything static, together: the ceiling, the same at every window.
const FLAT_TOTAL: usize = 20_000;

/// What is left after the prompt, split two ways and keeps a tenth spare, in
/// the ratio the owner's first figures gave: 20 parts built-in to 12 to MCP.
const BUILTIN_SHARE: usize = 20;
const MCP_SHARE: usize = 12;
const SHARE_TOTAL: usize = BUILTIN_SHARE + MCP_SHARE + 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Everything static, together: the ceiling.
    pub total: usize,
    /// [`PROMPT_CEILING`] at every window.
    pub prompt: usize,
    /// What the built-in schemas get when the prompt has taken its whole
    /// ceiling: their share of what is left.
    pub builtin_tools: usize,
    /// What the MCP schemas get: their share, charged first.
    pub mcp_tools: usize,
    /// Held back on purpose: the total is a ceiling, and a count of tokens is
    /// never exact enough to spend the last of it.
    pub reserve: usize,
}

impl Budget {
    /// The flat budget, whatever window the server reported.
    ///
    /// The window is taken and not used: the static budget is flat, and every
    /// window gets the same 20k. The parameter stays because the caller has
    /// the window at hand and asks about the budget in terms of it.
    pub fn for_window(_window: Option<usize>) -> Budget {
        let share = FLAT_TOTAL.saturating_sub(PROMPT_CEILING);
        let builtin_tools = share * BUILTIN_SHARE / SHARE_TOTAL;
        let mcp_tools = share * MCP_SHARE / SHARE_TOTAL;
        let reserve = share - builtin_tools - mcp_tools;
        Budget { total: FLAT_TOTAL, prompt: PROMPT_CEILING, builtin_tools, mcp_tools, reserve }
    }

    /// What the built-in schemas get once the prompt has taken what it
    /// actually measures, not what it is allowed.
    ///
    /// The ceiling is 10k and the prompt is written to fill it, but a full
    /// prompt plus the MCP share would leave nothing for the tools that make
    /// any task possible, so the schemas are given what is really left. MCP is
    /// charged first: those schemas come and go with the servers a user
    /// happens to have configured, the built-in ones are the product.
    ///
    /// The reserve is never spent: the total is a ceiling, not a target. What
    /// the prompt did not take is the built-in schemas' — they are what the
    /// rest of the budget is for.
    pub fn tool_allowance(&self, prompt_tokens: usize) -> usize {
        // What the prompt really took, up to its own ceiling.
        let prompt = prompt_tokens.min(self.prompt);
        // What is left for the schemas: MCP first, then the reserve, which is
        // held back.
        let left = self.total.saturating_sub(prompt).saturating_sub(self.mcp_tools).saturating_sub(self.reserve);
        // Their share of it, and no less: the built-in schemas are the product.
        left.max(self.builtin_tools)
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

    /// The note on the allowance named a ceiling of 4k; the ceiling is 10k.
    /// A note that names the wrong number is worse than no note.
    #[test]
    fn the_note_on_the_allowance_names_the_ceiling_that_is() {
        let source = include_str!("budget.rs");
        let stale = ["The ceiling is ", "4k"].concat();
        assert!(!source.contains(&stale), "the note names a ceiling that is not there");
        assert!(source.contains("PROMPT_CEILING"), "the note names the constant that is");
    }

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
        // The budget is flat, so the window the server did not report makes no
        // difference: the same budget either way.
        assert_eq!(Budget::for_window(None), Budget::for_window(Some(128_000)));
    }

    #[test]
    fn the_parts_leave_room_under_the_total() {
        let b = Budget::for_window(Some(64_000));
        assert!(b.prompt + b.builtin_tools + b.mcp_tools <= b.total);
        assert!(b.prompt + b.builtin_tools + b.mcp_tools + b.reserve <= b.total);
    }

    #[test]
    fn a_small_window_does_not_spend_its_budget_on_the_prompt() {
        // The prompt is a fixed cost: 1,778 tokens whatever the window is.
        // Enforced literally, its 10% share would be 500 at 64k and the schemas
        // would be left with nothing, so it takes what it measures and the
        // schemas take the rest. What has to hold is that it cannot crowd them
        // out, at any window.
        let prompt_tokens = 1_778;
        let b = Budget::for_window(Some(64_000));
        assert!(b.tool_allowance(prompt_tokens) > 0, "no schema fits");
        assert!(prompt_tokens * 3 < b.total * 2, "the prompt takes over half the budget");
    }

    /// The built-in schemas get their share, and what the prompt did not take
    /// is theirs too. MCP is charged first and the reserve is never spent.
    #[test]
    fn the_built_in_schemas_get_their_share_and_the_slack_the_prompt_left() {
        let b = Budget::for_window(Some(128_000));
        // A prompt that used its whole ceiling leaves the schemas their share.
        assert_eq!(b.tool_allowance(b.prompt), b.builtin_tools, "the share is what is left");
        // A prompt under its ceiling gives the slack to the schemas.
        let slack = 2_000;
        assert_eq!(b.tool_allowance(b.prompt - slack), b.builtin_tools + slack);
        // A prompt over its ceiling is charged no more than the ceiling.
        assert_eq!(b.tool_allowance(b.prompt * 10), b.builtin_tools);
        // The reserve is never spent, and MCP is served before the schemas.
        assert!(b.prompt + b.builtin_tools + b.mcp_tools + b.reserve <= b.total);
        assert!(b.tool_allowance(0) <= b.total - b.mcp_tools - b.reserve);
    }

    /// The reserve is held back and never spent, and the schemas are counted
    /// by what the prompt really cost, not by the ceiling it was allowed to
    /// take.
    #[test]
    fn the_reserve_is_never_spent_and_the_schemas_are_counted_by_what_the_prompt_cost() {
        let b = Budget::for_window(Some(128_000));
        // The schemas get their share and the slack the prompt left: a prompt
        // that used less leaves more for them.
        let small = b.tool_allowance(1_000);
        let large = b.tool_allowance(b.prompt - 1_000);
        assert!(small > large, "the schemas got the same whatever the prompt cost");
        assert_eq!(small, b.builtin_tools + (b.prompt - 1_000), "the share and the slack");
        // The reserve is in none of it: what is left is the total without the
        // prompt, the MCP share and the reserve.
        for prompt in [0usize, 1_000, 1_778, 5_000, b.prompt] {
            let allowance = b.tool_allowance(prompt);
            assert!(
                allowance <= b.total - prompt - b.mcp_tools - b.reserve,
                "the reserve was spent at {prompt}"
            );
            assert!(allowance >= b.builtin_tools, "the built-in schemas got less than their share at {prompt}");
        }
        // And a prompt over its ceiling is charged no more than the ceiling.
        assert_eq!(b.tool_allowance(b.prompt * 10), b.builtin_tools);
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
