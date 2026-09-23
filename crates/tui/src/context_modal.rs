//! `/context` breakdown modal.

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
        let inner_w = width.saturating_sub(6).clamp(20, 76);

        let title = " Context usage ";
        let dash_count = inner_w.saturating_sub(title.chars().count() + 1);
        lines.push((
            LineKind::System,
            format!("  {border_color}╭─\x1b[1;38;2;225;175;95m{title}{border_color}{}╮{reset}", "─".repeat(dash_count)),
        ));

        let used = self.usage.total_used();
        let cap = self.usage.total_capacity;
        let pct = self.usage.percentage();
        let rem = self.usage.remaining();

        let pad_row = |content: &str| -> String {
            let clipped = crate::tool_views::clip_ellipsis(content, inner_w.saturating_sub(2));
            let pad = " ".repeat(inner_w.saturating_sub(crate::visible_width(&clipped) + 2));
            format!("  {border_color}│{reset} {clipped}{pad} {border_color}│{reset}")
        };

        lines.push((LineKind::System, pad_row(&format!(
            "\x1b[1;38;2;240;235;225mTotal window:\x1b[0m       \x1b[38;2;200;195;185m{:>7} tokens\x1b[0m ({})",
            cap,
            ContextUsage::format_tokens(cap)
        ))));
        lines.push((LineKind::System, pad_row(&format!(
            "\x1b[1;38;2;240;235;225mTotal used:\x1b[0m         \x1b[38;2;225;175;95m{:>7} tokens\x1b[0m ({:.1}%)",
            used,
            pct
        ))));
        lines.push((LineKind::System, pad_row(&format!(
            "\x1b[1;38;2;240;235;225mRemaining free:\x1b[0m     \x1b[38;2;145;205;140m{:>7} tokens\x1b[0m ({:.1}%)",
            rem,
            (100.0 - pct).max(0.0)
        ))));

        lines.push((LineKind::System, pad_row("")));
        lines.push((LineKind::System, pad_row("\x1b[1;38;2;180;175;165mBy component\x1b[0m")));

        let items = [
            ("System prompt", self.usage.system_tokens),
            ("Memory and project", self.usage.memory_tokens),
            ("Tool definitions", self.usage.tools_tokens),
            ("User prompts", self.usage.user_tokens),
            ("Assistant replies", self.usage.assistant_tokens),
            ("Reasoning content", self.usage.reasoning_tokens),
            ("Tool outputs", self.usage.tool_output_tokens),
        ];

        // Label, count and share take 45 columns; the bar gets what is left, up to 16.
        let bar_w = inner_w.saturating_sub(2 + 45).min(16);
        for (label, count) in items {
            let item_pct = if cap > 0 { (count as f32 / cap as f32) * 100.0 } else { 0.0 };
            let bar_len = ((item_pct / 100.0) * bar_w as f32).round() as usize;
            let bar = "█".repeat(bar_len.min(bar_w));
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
        lines.push((LineKind::System, pad_row(&crate::key_hints(&[("Esc", "close")], inner_w.saturating_sub(2)))));

        lines.push((
            LineKind::System,
            format!("  {border_color}╰{}╯{reset}", "─".repeat(inner_w)),
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
        assert!(joined.contains("Context usage"));
        assert!(joined.contains("Total window:"));
        assert!(joined.contains("System prompt"));
        assert!(crate::strip_ansi(&joined).contains("Esc close"));
    }

    #[test]
    fn every_row_of_the_box_is_as_wide_as_its_top() {
        // The top border used to be one column short.
        let mut usage = ContextUsage::new(128_000);
        // A full bar on a narrow window pushed its row past the frame.
        usage.tool_output_tokens = 120_000;
        let modal = ContextModal::new(usage);
        for width in [40usize, 60, 80, 110, 140] {
            let widths: std::collections::BTreeSet<usize> =
                modal.render(width).iter().map(|l| crate::visible_width(&l.1)).filter(|w| *w > 0).collect();
            assert_eq!(widths.len(), 1, "{width} columns: rows of widths {widths:?}");
        }
    }
}
