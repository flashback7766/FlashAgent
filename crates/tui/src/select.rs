//! Generic interactive selection and menu primitives for the TUI:
//! settings, model picker, interactive questions, and tool confirmation.

use flashagent_core::Decision;

use crate::{clip_ansi, visible_width, LineKind, RenderLine};

/// Horizontal confirmation selector (e.g. Allow / Deny).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmSelect {
    pub selected: Decision,
}

impl Default for ConfirmSelect {
    fn default() -> Self {
        Self { selected: Decision::Allow }
    }
}

impl ConfirmSelect {
    /// Create a new selector defaulted to `Decision::Allow`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Toggle between Allow and Deny.
    pub fn toggle(&mut self) {
        self.selected = match self.selected {
            Decision::Allow => Decision::Deny,
            Decision::Deny => Decision::Allow,
        };
    }

    /// Move selection to Allow.
    pub fn left(&mut self) {
        self.selected = Decision::Allow;
    }

    /// Move selection to Deny.
    pub fn right(&mut self) {
        self.selected = Decision::Deny;
    }

    /// Get current decision.
    pub fn decision(self) -> Decision {
        self.selected
    }
}

/// Single item in a select menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectItem<T> {
    pub label: String,
    pub description: Option<String>,
    pub value: T,
}

impl<T> SelectItem<T> {
    /// Create a simple item without description.
    pub fn new(label: impl Into<String>, value: T) -> Self {
        Self {
            label: label.into(),
            description: None,
            value,
        }
    }

    /// Create an item with a descriptive subtitle.
    pub fn with_description(label: impl Into<String>, desc: impl Into<String>, value: T) -> Self {
        Self {
            label: label.into(),
            description: Some(desc.into()),
            value,
        }
    }
}

/// Generic vertical select menu navigated with Up / Down / Enter / Esc,
/// with support for scrolling window (10 items), paging, and live search filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectMenu<T> {
    pub title: String,
    pub items: Vec<SelectItem<T>>,
    pub selected: usize,
    pub filter: String,
    pub page_size: usize,
}

impl<T> SelectMenu<T> {
    /// Create a new selection menu with given title and items.
    pub fn new(title: impl Into<String>, items: Vec<SelectItem<T>>) -> Self {
        Self {
            title: title.into(),
            items,
            selected: 0,
            filter: String::new(),
            page_size: 10,
        }
    }

    /// Return 0-based indices of items matching the current search filter.
    pub fn filtered_indices(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.items.len()).collect();
        }
        let q = self.filter.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                it.label.to_lowercase().contains(&q)
                    || it.description.as_ref().is_some_and(|d| d.to_lowercase().contains(&q))
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Move selection up within filtered items (wraps around).
    pub fn up(&mut self) {
        let matches = self.filtered_indices();
        if matches.is_empty() {
            return;
        }
        let pos = matches.iter().position(|&idx| idx == self.selected).unwrap_or(0);
        let new_pos = if pos == 0 { matches.len() - 1 } else { pos - 1 };
        self.selected = matches[new_pos];
    }

    /// Move selection down within filtered items (wraps around).
    pub fn down(&mut self) {
        let matches = self.filtered_indices();
        if matches.is_empty() {
            return;
        }
        let pos = matches.iter().position(|&idx| idx == self.selected).unwrap_or(0);
        let new_pos = (pos + 1) % matches.len();
        self.selected = matches[new_pos];
    }

    /// Jump up by a page (default 10 items).
    pub fn page_up(&mut self) {
        let matches = self.filtered_indices();
        if matches.is_empty() {
            return;
        }
        let pos = matches.iter().position(|&idx| idx == self.selected).unwrap_or(0);
        let new_pos = pos.saturating_sub(self.page_size);
        self.selected = matches[new_pos];
    }

    /// Jump down by a page (default 10 items).
    pub fn page_down(&mut self) {
        let matches = self.filtered_indices();
        if matches.is_empty() {
            return;
        }
        let pos = matches.iter().position(|&idx| idx == self.selected).unwrap_or(0);
        let new_pos = (pos + self.page_size).min(matches.len() - 1);
        self.selected = matches[new_pos];
    }

    /// Append character to live search filter.
    pub fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
        let matches = self.filtered_indices();
        if !matches.is_empty() && !matches.contains(&self.selected) {
            self.selected = matches[0];
        }
    }

    /// Remove last character from live search filter.
    pub fn pop_filter_char(&mut self) {
        self.filter.pop();
        let matches = self.filtered_indices();
        if !matches.is_empty() && !matches.contains(&self.selected) {
            self.selected = matches[0];
        }
    }

    /// Currently selected item reference.
    pub fn selected_item(&self) -> Option<&SelectItem<T>> {
        self.items.get(self.selected)
    }

    /// Value of currently selected item.
    pub fn selected_value(&self) -> Option<&T> {
        self.items.get(self.selected).map(|it| &it.value)
    }

    /// Select an item by value if present.
    pub fn select_by_value(&mut self, val: &T)
    where
        T: PartialEq,
    {
        if let Some(idx) = self.items.iter().position(|it| &it.value == val) {
            self.selected = idx;
        }
    }

    /// Render menu card to lines fitting `width`, constrained to `page_size` visible items.
    pub fn render(&self, width: usize) -> Vec<RenderLine> {
        let card_w = width.clamp(36, 74);
        let inner_w = card_w.saturating_sub(2);
        let border_color = "\x1b[38;2;95;90;85m";
        let reset = "\x1b[0m";

        let pad_row = |content: &str| -> String {
            let vis = visible_width(content);
            let clipped = if vis > inner_w {
                clip_ansi(content, inner_w)
            } else {
                content.to_string()
            };
            let clipped_vis = visible_width(&clipped);
            let pad = inner_w.saturating_sub(clipped_vis);
            format!("{border_color}│{reset}{clipped}{}{border_color}│{reset}", " ".repeat(pad))
        };

        let mut lines = Vec::new();
        let matches = self.filtered_indices();
        let total_matches = matches.len();

        // Top border with title, item counter, and live filter indicator
        let count_str = if self.filter.is_empty() {
            format!("({} models)", self.items.len())
        } else {
            format!("({total_matches}/{} · \"{}\")", self.items.len(), self.filter)
        };
        let title_styled = format!(" \x1b[1;38;2;225;175;95m{}\x1b[0m \x1b[38;2;160;155;145m{count_str}\x1b[0m ", self.title);
        let title_vis = visible_width(&title_styled);
        let border_dashes = inner_w.saturating_sub(title_vis);
        lines.push((
            LineKind::System,
            format!("{border_color}╭─{title_styled}{}╮{reset}", "─".repeat(border_dashes.saturating_sub(1))),
        ));

        if matches.is_empty() {
            lines.push((LineKind::System, pad_row("  \x1b[38;2;135;130;125m(No matching models found)\x1b[0m")));
        } else {
            let cur_pos = matches.iter().position(|&idx| idx == self.selected).unwrap_or(0);
            let page_size = self.page_size;
            let start = if cur_pos < page_size {
                0
            } else {
                (cur_pos + 1).saturating_sub(page_size)
            };
            let end = (start + page_size).min(total_matches);

            if start > 0 {
                lines.push((
                    LineKind::System,
                    pad_row(&format!("  \x1b[38;2;135;130;125m▲ ... ({} more above) ...\x1b[0m", start)),
                ));
            }

            for &idx in &matches[start..end] {
                let item = &self.items[idx];
                let is_cur = idx == self.selected;
                let (cursor, label_styled) = if is_cur {
                    (" \x1b[1;38;2;225;175;95m❯\x1b[0m", format!("\x1b[1;38;2;245;240;232m{}\x1b[0m", item.label))
                } else {
                    ("  ", format!("\x1b[38;2;160;155;145m{}\x1b[0m", item.label))
                };
                lines.push((LineKind::System, pad_row(&format!("{cursor} {label_styled}"))));
                if let Some(desc) = &item.description {
                    lines.push((
                        LineKind::System,
                        pad_row(&format!("    \x1b[38;2;135;130;125m{desc}\x1b[0m")),
                    ));
                }
            }

            if end < total_matches {
                lines.push((
                    LineKind::System,
                    pad_row(&format!("  \x1b[38;2;135;130;125m▼ ... ({} more below) ...\x1b[0m", total_matches - end)),
                ));
            }
        }

        // Bottom border with key hints
        lines.push((
            LineKind::System,
            format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)),
        ));
        lines.push((
            LineKind::System,
            "  \x1b[38;2;135;130;125m↑/↓ · PgUp/PgDn · type to search · enter select · esc cancel\x1b[0m".to_string(),
        ));

        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_select_navigation() {
        let mut cs = ConfirmSelect::new();
        assert_eq!(cs.decision(), Decision::Allow);

        cs.right();
        assert_eq!(cs.decision(), Decision::Deny);

        cs.left();
        assert_eq!(cs.decision(), Decision::Allow);

        cs.toggle();
        assert_eq!(cs.decision(), Decision::Deny);

        cs.toggle();
        assert_eq!(cs.decision(), Decision::Allow);
    }

    #[test]
    fn select_menu_navigation_and_render() {
        let items = vec![
            SelectItem::with_description("Model A", "Small local model", "a"),
            SelectItem::with_description("Model B", "Large reasoning model", "b"),
            SelectItem::new("Model C", "c"),
        ];
        let mut menu = SelectMenu::new("Select Model", items);
        assert_eq!(menu.selected_value(), Some(&"a"));

        menu.down();
        assert_eq!(menu.selected_value(), Some(&"b"));

        menu.down();
        assert_eq!(menu.selected_value(), Some(&"c"));

        menu.down(); // wraps around to 0
        assert_eq!(menu.selected_value(), Some(&"a"));

        menu.up(); // wraps around to last
        assert_eq!(menu.selected_value(), Some(&"c"));

        menu.select_by_value(&"b");
        assert_eq!(menu.selected_value(), Some(&"b"));

        let lines = menu.render(80);
        assert!(lines.iter().any(|(_, t)| t.contains("Select Model")));
        assert!(lines.iter().any(|(_, t)| t.contains("Model A")));
        assert!(lines.iter().any(|(_, t)| t.contains("Model B")));
        assert!(lines.iter().any(|(_, t)| t.contains("↑/↓")));
    }

    #[test]
    fn test_select_menu_scrolling_and_search() {
        let items: Vec<_> = (0..50)
            .map(|i| SelectItem::new(format!("model-{i:02}"), format!("m{i}")))
            .collect();
        let mut menu = SelectMenu::new("Models", items);
        assert_eq!(menu.page_size, 10);

        let lines = menu.render(80);
        assert!(lines.iter().any(|(_, t)| t.contains("▼ ... (40 more below)")));

        menu.push_filter_char('4');
        let filtered = menu.filtered_indices();
        assert!(filtered.len() >= 5);
        menu.pop_filter_char();
        menu.selected = 0;
        menu.page_down();
        assert_eq!(menu.selected, 10);
        menu.page_up();
        assert_eq!(menu.selected, 0);
    }
}
