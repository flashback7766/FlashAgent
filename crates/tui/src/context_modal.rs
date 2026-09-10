//! Detailed context window breakdown modal for `/context`.

use flashagent_core::ContextUsage;
use crate::{LineKind, RenderLine};

pub struct ContextModal {
    pub usage: ContextUsage,
}

impl ContextModal {
    pub fn new(usage: ContextUsage) -> Self {
        Self { usage }
    }

    pub fn render(&self, width: usize) -> Vec<RenderLine> {
        let mut lines = Vec::new();
        let border_color = "\x1b[38;2;100;95;90m";
        let reset = "\x1b[0m";
        let inner_w = width.saturating_sub(6).clamp(40, 76);

        let title = " Context Window Breakdown (/context) ";
        let dash_count = inner_w.saturating_sub(title.chars().count() + 2);
        lines.push((
            LineKind::System,
            format!("  {border_color}┌─\x1b[1;38;2;225;175;95m{title}{border_color}{}┐{reset}", "─".repeat(dash_count)),
        ));

        let used = self.usage.total_used();
        let cap = self.usage.total_capacity;
        let pct = self.usage.percentage();
        let rem = self.usage.remaining();

        let pad_row = |content: &str| -> String {
            let vis = crate::visible_width(content);
            let pad = " ".repeat(inner_w.saturating_sub(vis + 2));
            format!("  {border_color}│{reset} {content}{pad} {border_color}│{reset}")
        };

        lines.push((LineKind::System, pad_row(&format!(
            "\x1b[1;38;2;240;235;225mTotal Window:\x1b[0m       \x1b[38;2;200;195;185m{:>7} tokens\x1b[0m ({})",
            cap,
            ContextUsage::format_tokens(cap)
        ))));
        lines.push((LineKind::System, pad_row(&format!(
            "\x1b[1;38;2;240;235;225mTotal Used:\x1b[0m         \x1b[38;2;225;175;95m{:>7} tokens\x1b[0m ({:.1}%)",
            used,
            pct
        ))));
        lines.push((LineKind::System, pad_row(&format!(
            "\x1b[1;38;2;240;235;225mRemaining Free:\x1b[0m     \x1b[38;2;145;205;140m{:>7} tokens\x1b[0m ({:.1}%)",
            rem,
            (100.0 - pct).max(0.0)
        ))));

        lines.push((LineKind::System, pad_row("")));
        lines.push((LineKind::System, pad_row("\x1b[1;38;2;180;175;165mUsage by Component:\x1b[0m")));

        let items = [
            ("System Prompt", self.usage.system_tokens),
            ("Memory & Project", self.usage.memory_tokens),
            ("Tool Definitions", self.usage.tools_tokens),
            ("User Prompts", self.usage.user_tokens),
            ("Assistant Replies", self.usage.assistant_tokens),
            ("Reasoning Content", self.usage.reasoning_tokens),
            ("Tool Outputs", self.usage.tool_output_tokens),
        ];

        for (label, count) in items {
            let item_pct = if cap > 0 { (count as f32 / cap as f32) * 100.0 } else { 0.0 };
            let bar_len = ((item_pct / 100.0) * 16.0).round() as usize;
            let bar = "█".repeat(bar_len);
            lines.push((LineKind::System, pad_row(&format!(
                "  \x1b[38;2;160;155;145m{:<18}\x1b[0m {:>6} tokens  \x1b[38;2;135;130;125m({:>4.1}%)\x1b[0m  \x1b[38;2;175;170;225m{bar}\x1b[0m",
                label,
                count,
                item_pct
            ))));
        }

        lines.push((LineKind::System, pad_row("")));
        let gauge = self.usage.format_compact_gauge(20);
        lines.push((LineKind::System, pad_row(&format!("Gauge: {gauge}"))));
        lines.push((LineKind::System, pad_row("\x1b[38;2;135;130;125m(press F1, Enter or Esc to dismiss)\x1b[0m")));

        lines.push((
            LineKind::System,
            format!("  {border_color}└{}┘{reset}", "─".repeat(inner_w)),
        ));

        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_modal_render() {
        let mut usage = ContextUsage::new(128_000);
        usage.system_tokens = 1_000;
        usage.user_tokens = 5_000;
        let modal = ContextModal::new(usage);
        let lines = modal.render(80);
        assert!(!lines.is_empty());
        let joined = lines.iter().map(|l| &l.1).cloned().collect::<Vec<_>>().join("\n");
        assert!(joined.contains("Context Window Breakdown"));
        assert!(joined.contains("Total Window:"));
        assert!(joined.contains("System Prompt"));
    }
}
