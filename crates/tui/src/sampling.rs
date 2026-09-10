//! Interactive Sampling Parameters menu opened via `/sampling`, `F5`, or Settings.
//! Allows live reconfiguration of sampling preset, temperature, top_p, top_k, repeat_penalty, presence_penalty, and min_p.

use crossterm::event::{KeyCode, KeyModifiers};
use flashagent_core::{AppConfig, SamplingPreset};
use crate::{LineKind, RenderLine};

/// Action returned by the sampling view on key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SamplingAction {
    /// Keep sampling menu open.
    None,
    /// Close menu without applying changes.
    Close,
    /// Apply changes, save config, and close menu.
    SaveAndClose,
}

pub struct SamplingView {
    pub preset: SamplingPreset,
    pub selected_index: usize,
    pub temp_buf: String,
    pub top_p_buf: String,
    pub top_k_buf: String,
    pub repeat_penalty_buf: String,
    pub presence_penalty_buf: String,
    pub min_p_buf: String,
    pub is_dirty: bool,
}

impl SamplingView {
    pub fn new(config: &AppConfig) -> Self {
        let preset = config.sampling_preset;
        let mut view = Self {
            preset,
            selected_index: 0,
            temp_buf: format!("{:.2}", config.temperature),
            top_p_buf: format!("{:.2}", config.top_p.unwrap_or(0.95)),
            top_k_buf: format!("{}", config.top_k.unwrap_or(20)),
            repeat_penalty_buf: format!("{:.2}", config.repeat_penalty.unwrap_or(1.00)),
            presence_penalty_buf: format!("{:.2}", config.presence_penalty.unwrap_or(0.00)),
            min_p_buf: format!("{:.2}", config.min_p.unwrap_or(0.00)),
            is_dirty: false,
        };
        // If preset is not custom, align buffers to standard preset values
        if preset != SamplingPreset::Custom {
            view.sync_buffers_to_preset(preset);
        }
        view
    }

    pub fn total_items(&self) -> usize {
        8 // 0: Preset, 1: Temp, 2: Top P, 3: Top K, 4: Repeat Penalty, 5: Presence Penalty, 6: Min P, 7: Apply
    }

    fn sync_buffers_to_preset(&mut self, preset: SamplingPreset) {
        self.preset = preset;
        if preset != SamplingPreset::Custom {
            let (temp, top_p, top_k, rp, pp, mp) = preset.values();
            self.temp_buf = format!("{:.2}", temp);
            self.top_p_buf = format!("{:.2}", top_p);
            self.top_k_buf = format!("{top_k}");
            self.repeat_penalty_buf = format!("{:.2}", rp);
            self.presence_penalty_buf = format!("{:.2}", pp);
            self.min_p_buf = format!("{:.2}", mp);
        }
    }

    pub fn clamp_buffers(&mut self) {
        let temp = self.temp_buf.parse::<f32>().unwrap_or(0.60).clamp(0.0, 2.0);
        self.temp_buf = format!("{:.2}", temp);

        let top_p = self.top_p_buf.parse::<f32>().unwrap_or(0.95).clamp(0.0, 1.0);
        self.top_p_buf = format!("{:.2}", top_p);

        let top_k = self.top_k_buf.parse::<u32>().unwrap_or(20).clamp(0, 500);
        self.top_k_buf = format!("{}", top_k);

        let rp = self.repeat_penalty_buf.parse::<f32>().unwrap_or(1.00).clamp(0.0, 2.0);
        self.repeat_penalty_buf = format!("{:.2}", rp);

        let pp = self.presence_penalty_buf.parse::<f32>().unwrap_or(0.00).clamp(-2.0, 2.0);
        self.presence_penalty_buf = format!("{:.2}", pp);

        let mp = self.min_p_buf.parse::<f32>().unwrap_or(0.00).clamp(0.0, 1.0);
        self.min_p_buf = format!("{:.2}", mp);
    }

    pub fn apply_to_config(&mut self, config: &mut AppConfig) {
        self.clamp_buffers();
        config.sampling_preset = self.preset;
        config.temperature = self.temp_buf.parse::<f32>().unwrap_or(0.60);
        config.top_p = Some(self.top_p_buf.parse::<f32>().unwrap_or(0.95));
        config.top_k = Some(self.top_k_buf.parse::<u32>().unwrap_or(20));
        config.repeat_penalty = Some(self.repeat_penalty_buf.parse::<f32>().unwrap_or(1.00));
        config.presence_penalty = Some(self.presence_penalty_buf.parse::<f32>().unwrap_or(0.00));
        config.min_p = Some(self.min_p_buf.parse::<f32>().unwrap_or(0.00));
    }

    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> SamplingAction {
        match code {
            KeyCode::Esc => {
                self.clamp_buffers();
                SamplingAction::Close
            }
            KeyCode::Up => {
                self.clamp_buffers();
                let total = self.total_items();
                self.selected_index = (self.selected_index + total - 1) % total;
                SamplingAction::None
            }
            KeyCode::Down | KeyCode::Tab => {
                self.clamp_buffers();
                let total = self.total_items();
                self.selected_index = (self.selected_index + 1) % total;
                SamplingAction::None
            }
            KeyCode::BackTab => {
                self.clamp_buffers();
                let total = self.total_items();
                self.selected_index = (self.selected_index + total - 1) % total;
                SamplingAction::None
            }
            KeyCode::Left => {
                match self.selected_index {
                    0 => {
                        self.cycle_preset(-1);
                    }
                    1 => self.adjust_float(1, -0.05, 0.0, 2.0),
                    2 => self.adjust_float(2, -0.05, 0.0, 1.0),
                    3 => self.adjust_int(3, -5, 0, 500),
                    4 => self.adjust_float(4, -0.05, 0.0, 2.0),
                    5 => self.adjust_float(5, -0.05, -2.0, 2.0),
                    6 => self.adjust_float(6, -0.05, 0.0, 1.0),
                    _ => {}
                }
                SamplingAction::None
            }
            KeyCode::Right => {
                match self.selected_index {
                    0 => {
                        self.cycle_preset(1);
                    }
                    1 => self.adjust_float(1, 0.05, 0.0, 2.0),
                    2 => self.adjust_float(2, 0.05, 0.0, 1.0),
                    3 => self.adjust_int(3, 5, 0, 500),
                    4 => self.adjust_float(4, 0.05, 0.0, 2.0),
                    5 => self.adjust_float(5, 0.05, -2.0, 2.0),
                    6 => self.adjust_float(6, 0.05, 0.0, 1.0),
                    _ => {}
                }
                SamplingAction::None
            }
            KeyCode::Backspace => {
                if self.selected_index >= 1 && self.selected_index <= 6 {
                    let buf = self.active_buffer_mut();
                    buf.pop();
                    self.preset = SamplingPreset::Custom;
                    self.is_dirty = true;
                }
                SamplingAction::None
            }
            KeyCode::Enter => {
                self.clamp_buffers();
                SamplingAction::SaveAndClose
            }
            KeyCode::Char(' ') => {
                if self.selected_index == 0 {
                    self.cycle_preset(1);
                } else if self.selected_index == 7 {
                    self.clamp_buffers();
                    return SamplingAction::SaveAndClose;
                }
                SamplingAction::None
            }
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
                if (1..=6).contains(&self.selected_index) && (c.is_ascii_digit() || c == '.' || c == '-') {
                    let buf = self.active_buffer_mut();
                    if c == '.' && buf.contains('.') {
                        // ignore duplicate dot
                    } else if c == '-' && !buf.is_empty() {
                        // ignore minus not at head
                    } else {
                        buf.push(c);
                        self.preset = SamplingPreset::Custom;
                        self.is_dirty = true;
                    }
                }
                SamplingAction::None
            }
            _ => SamplingAction::None,
        }
    }

    fn active_buffer_mut(&mut self) -> &mut String {
        match self.selected_index {
            1 => &mut self.temp_buf,
            2 => &mut self.top_p_buf,
            3 => &mut self.top_k_buf,
            4 => &mut self.repeat_penalty_buf,
            5 => &mut self.presence_penalty_buf,
            6 => &mut self.min_p_buf,
            _ => &mut self.temp_buf,
        }
    }

    fn cycle_preset(&mut self, delta: i32) {
        let all = SamplingPreset::all();
        let cur_idx = all.iter().position(|&p| p == self.preset).unwrap_or(0);
        let next_idx = if delta < 0 {
            (cur_idx + all.len() - 1) % all.len()
        } else {
            (cur_idx + 1) % all.len()
        };
        let new_preset = all[next_idx];
        self.sync_buffers_to_preset(new_preset);
        self.is_dirty = true;
    }

    fn adjust_float(&mut self, idx: usize, delta: f32, min_val: f32, max_val: f32) {
        let buf = match idx {
            1 => &mut self.temp_buf,
            2 => &mut self.top_p_buf,
            4 => &mut self.repeat_penalty_buf,
            5 => &mut self.presence_penalty_buf,
            6 => &mut self.min_p_buf,
            _ => return,
        };
        let cur = buf.parse::<f32>().unwrap_or(min_val);
        let updated = (cur + delta).clamp(min_val, max_val);
        *buf = format!("{:.2}", updated);
        self.preset = SamplingPreset::Custom;
        self.is_dirty = true;
    }

    fn adjust_int(&mut self, idx: usize, delta: i32, min_val: u32, max_val: u32) {
        if idx != 3 {
            return;
        }
        let cur = self.top_k_buf.parse::<i32>().unwrap_or(20);
        let updated = (cur + delta).clamp(min_val as i32, max_val as i32) as u32;
        self.top_k_buf = format!("{}", updated);
        self.preset = SamplingPreset::Custom;
        self.is_dirty = true;
    }

    pub fn render(&self, width: usize) -> Vec<RenderLine> {
        let mut lines = Vec::new();
        let border_color = "\x1b[38;2;100;95;90m";
        let reset = "\x1b[0m";
        let inner_w = width.saturating_sub(6).clamp(20, 110);

        let title = " Sampling Parameters (/sampling · F5) ";
        let dash_count = inner_w.saturating_sub(title.chars().count() + 1);
        lines.push((
            LineKind::System,
            format!("  {border_color}┌─\x1b[1;38;2;225;175;95m{title}{border_color}{}┐{reset}", "─".repeat(dash_count)),
        ));

        let pad_row = |content: &str| -> String {
            let max_w = inner_w.saturating_sub(2);
            let vis = crate::visible_width(content);
            let clipped = if vis > max_w {
                crate::clip_ansi(content, max_w)
            } else {
                content.to_string()
            };
            let clipped_vis = crate::visible_width(&clipped);
            let pad = " ".repeat(max_w.saturating_sub(clipped_vis));
            format!("  {border_color}│{reset} {clipped}{pad} {border_color}│{reset}")
        };

        lines.push((LineKind::System, pad_row("\x1b[38;2;135;130;125mConfigure LLM temperature, top-p/k, penalty, and min-p sampling\x1b[0m")));
        lines.push((LineKind::System, pad_row("")));

        // Row 0: Preset
        let is_preset_sel = self.selected_index == 0;
        let ptr0 = if is_preset_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
        let preset_val = format!("[ {} ]", self.preset.label());
        let (label0, val0) = if is_preset_sel {
            (
                format!("\x1b[1;38;2;240;235;225m{:<18}\x1b[0m", "Preset"),
                format!("\x1b[1;38;2;225;175;95m{:<22}\x1b[0m \x1b[38;2;135;130;125m(←/→ to cycle preset)\x1b[0m", preset_val),
            )
        } else {
            (
                format!("\x1b[38;2;160;155;145m{:<18}\x1b[0m", "Preset"),
                format!("\x1b[38;2;200;195;185m{:<22}\x1b[0m", preset_val),
            )
        };
        lines.push((LineKind::System, pad_row(&format!("{ptr0} {label0} {val0}"))));

        // Rows 1..6: Parameters
        let fields = [
            (1, "Temperature", &self.temp_buf, "0.00..2.00 (coding: 0.60, mtp: 0.30, chat: 0.80, precise: 0.10)"),
            (2, "Top P", &self.top_p_buf, "0.00..1.00 (coding: 0.95, mtp: 0.90, precise: 0.75)"),
            (3, "Top K", &self.top_k_buf, "0..500     (coding: 20, mtp: 40, chat: 50, precise: 10)"),
            (4, "Repeat Penalty", &self.repeat_penalty_buf, "0.00..2.00 (coding: 1.00, mtp: 1.02, gemma: 1.08, chat: 1.10)"),
            (5, "Presence Penalty", &self.presence_penalty_buf, "-2.00..2.00 (chat: 0.10, mtp-chat: 0.05, default: 0.00)"),
            (6, "Min P", &self.min_p_buf, "0.00..1.00 (mtp-code: 0.05, mtp: 0.03, default: 0.00)"),
        ];

        for (idx, label, buf, range_hint) in fields {
            let is_sel = self.selected_index == idx;
            let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
            let display_val = if is_sel { format!("{}█", buf) } else { buf.to_string() };
            let (label_styled, val_styled) = if is_sel {
                (
                    format!("\x1b[1;38;2;240;235;225m{:<18}\x1b[0m", label),
                    format!("\x1b[1;38;2;225;175;95m{:<10}\x1b[0m \x1b[38;2;135;130;125m{}\x1b[0m", display_val, range_hint),
                )
            } else {
                (
                    format!("\x1b[38;2;160;155;145m{:<18}\x1b[0m", label),
                    format!("\x1b[38;2;200;195;185m{:<10}\x1b[0m \x1b[38;2;100;95;90m{}\x1b[0m", display_val, range_hint),
                )
            };
            lines.push((LineKind::System, pad_row(&format!("{ptr} {label_styled} {val_styled}"))));
        }

        // Row 7: Apply & Save
        let is_apply_sel = self.selected_index == 7;
        let ptr7 = if is_apply_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
        let apply_styled = if is_apply_sel {
            "\x1b[1;38;2;225;175;95m[ Apply & Return to Chat (Enter) ]\x1b[0m"
        } else {
            "\x1b[38;2;160;155;145m[ Apply & Return to Chat (Enter) ]\x1b[0m"
        };
        lines.push((LineKind::System, pad_row("")));
        lines.push((LineKind::System, pad_row(&format!("{ptr7} {apply_styled}"))));

        lines.push((LineKind::System, pad_row("")));
        lines.push((LineKind::System, pad_row("\x1b[38;2;135;130;125mtype digits/./- · ←/→ adjust · ↑/↓ navigate · Enter apply · Esc back\x1b[0m")));

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
    fn test_sampling_preset_cycling() {
        let cfg = AppConfig::default();
        let mut view = SamplingView::new(&cfg);
        assert_eq!(view.preset, SamplingPreset::Coding);
        assert_eq!(view.temp_buf, "0.60");
        assert_eq!(view.top_p_buf, "0.95");
        assert_eq!(view.top_k_buf, "20");
        assert_eq!(view.min_p_buf, "0.00");

        // Cycle right on row 0 -> MtpCoding
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(view.preset, SamplingPreset::MtpCoding);
        assert_eq!(view.temp_buf, "0.30");
        assert_eq!(view.top_p_buf, "0.90");
        assert_eq!(view.top_k_buf, "40");
        assert_eq!(view.min_p_buf, "0.05");

        // Cycle right on row 0 -> Mtp
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(view.preset, SamplingPreset::Mtp);
        assert_eq!(view.temp_buf, "0.50");
        assert_eq!(view.top_p_buf, "0.92");
        assert_eq!(view.top_k_buf, "30");
        assert_eq!(view.min_p_buf, "0.03");

        // Cycle right on row 0 -> Chatting
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(view.preset, SamplingPreset::Chatting);
        assert_eq!(view.temp_buf, "0.80");
        assert_eq!(view.top_p_buf, "0.95");
        assert_eq!(view.top_k_buf, "50");
        assert_eq!(view.presence_penalty_buf, "0.10");

        // Cycle right on row 0 -> MtpChatting
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(view.preset, SamplingPreset::MtpChatting);
        assert_eq!(view.temp_buf, "0.65");
        assert_eq!(view.top_p_buf, "0.88");
        assert_eq!(view.presence_penalty_buf, "0.05");

        // Cycle right on row 0 -> Precise
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(view.preset, SamplingPreset::Precise);
        assert_eq!(view.temp_buf, "0.10");
        assert_eq!(view.top_p_buf, "0.75");
        assert_eq!(view.top_k_buf, "10");

        // Cycle right on row 0 -> Gemma
        view.handle_key(KeyCode::Right, KeyModifiers::empty());
        assert_eq!(view.preset, SamplingPreset::Gemma);
        assert_eq!(view.temp_buf, "1.00");
        assert_eq!(view.top_p_buf, "0.95");
        assert_eq!(view.top_k_buf, "64");
        assert_eq!(view.repeat_penalty_buf, "1.08");
        assert_eq!(view.presence_penalty_buf, "0.02");
        assert_eq!(view.min_p_buf, "0.03");
    }

    #[test]
    fn test_sampling_digit_typing_switches_to_custom() {
        let cfg = AppConfig::default();
        let mut view = SamplingView::new(&cfg);
        view.selected_index = 1; // Temperature
        view.temp_buf.clear();
        view.handle_key(KeyCode::Char('0'), KeyModifiers::empty());
        view.handle_key(KeyCode::Char('.'), KeyModifiers::empty());
        view.handle_key(KeyCode::Char('4'), KeyModifiers::empty());
        view.handle_key(KeyCode::Char('5'), KeyModifiers::empty());

        assert_eq!(view.temp_buf, "0.45");
        assert_eq!(view.preset, SamplingPreset::Custom);

        let mut out_cfg = cfg.clone();
        view.apply_to_config(&mut out_cfg);
        assert_eq!(out_cfg.sampling_preset, SamplingPreset::Custom);
        assert!((out_cfg.temperature - 0.45).abs() < 0.001);
    }
}
