//! What changed since the build you were running.
//!
//! An update that lands silently is an update nobody uses: the features are
//! there, and the person keeps working the old way. After the binary moves
//! forward by more than a hair, this shows what arrived — read straight out
//! of `CHANGELOG.md`, so the screen cannot drift from what actually shipped.

use crossterm::event::{Event, KeyCode, KeyEventKind};

/// Marks an entry that is a paragraph of prose rather than a listed change.
const PROSE: &str = "\u{b6}";

/// Marks the place where the changelog asks for a new screen.
const PAGE: &str = "\u{c}";

/// How the changelog asks for one. An HTML comment, so GitHub shows nothing.
const PAGE_MARK: &str = "<!-- page -->";

/// Presses it takes to get past a release's note. A note someone took the
/// time to write is not skipped by a key held down from the last screen.
const NOTE_PRESSES: usize = 3;

/// The changelog as it stood when this binary was built.
pub const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

/// One released version worth showing.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    /// Version tag, e.g. `b238`.
    pub version: String,
    /// Short title after the em dash, when the entry has one.
    pub title: String,
    /// Entries, in the order they were written.
    pub items: Vec<String>,
}

/// Sortable rank of a version: `b238` → (0, 238, 0), `v1.2.3` → (1, 2, 3).
///
/// Betas live below 1.0 on purpose: the move from `b238` to `v1.0.0` is the
/// one release where a user most wants to read what changed, and a plain
/// numeric comparison would call it a downgrade and say nothing.
fn rank(version: &str) -> Option<(u64, u64, u64)> {
    if let Some(n) = version.strip_prefix('b') {
        return n.parse().ok().map(|b| (0, b, 0));
    }
    let mut parts = version.strip_prefix('v')?.split(['.', '-']).map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)))
}

/// Collapse a wrapped changelog bullet back into one paragraph.
fn flatten(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Releases in `changelog` that are newer than `from` and no newer than `to`,
/// newest first.
///
/// `Unreleased` is skipped: it describes a build nobody has. An unparseable
/// version is skipped rather than guessed at.
pub fn releases_between(changelog: &str, from: &str, to: &str) -> Vec<Release> {
    let (Some(from_rank), Some(to_rank)) = (rank(from), rank(to)) else {
        return Vec::new();
    };
    if from_rank >= to_rank {
        return Vec::new();
    }

    let mut out: Vec<Release> = Vec::new();
    let mut current: Option<Release> = None;
    let mut item: Vec<String> = Vec::new();
    // A release can open with prose — a note from the maintainer — before
    // its list of changes. It is kept whole, as paragraphs.
    let mut paragraph: Vec<String> = Vec::new();

    let finish_item = |item: &mut Vec<String>, current: &mut Option<Release>| {
        if !item.is_empty() {
            if let Some(rel) = current.as_mut() {
                rel.items.push(flatten(item));
            }
            item.clear();
        }
    };
    let finish_paragraph = |paragraph: &mut Vec<String>, current: &mut Option<Release>| {
        if !paragraph.is_empty() {
            if let Some(rel) = current.as_mut() {
                rel.items.push(format!("{PROSE}{}", flatten(paragraph)));
            }
            paragraph.clear();
        }
    };

    for line in changelog.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            finish_item(&mut item, &mut current);
            finish_paragraph(&mut paragraph, &mut current);
            if let Some(rel) = current.take() {
                out.push(rel);
            }
            let (version, title) = match heading.split_once(" — ") {
                Some((v, t)) => (v.trim(), t.trim()),
                None => (heading.trim(), ""),
            };
            current = match rank(version) {
                Some(r) if r > from_rank && r <= to_rank => Some(Release {
                    version: version.to_string(),
                    title: title.to_string(),
                    items: Vec::new(),
                }),
                _ => None,
            };
            continue;
        }
        if current.is_none() {
            continue;
        }
        if line.trim() == PAGE_MARK {
            finish_item(&mut item, &mut current);
            finish_paragraph(&mut paragraph, &mut current);
            if let Some(rel) = current.as_mut() {
                rel.items.push(PAGE.to_string());
            }
            continue;
        }
        if let Some(sub) = line.strip_prefix("### ") {
            // Sub-headings become their own entry, so a long release still
            // reads as a list rather than one wall.
            finish_item(&mut item, &mut current);
            finish_paragraph(&mut paragraph, &mut current);
            if let Some(rel) = current.as_mut() {
                rel.items.push(format!("[{}]", sub.trim()));
            }
        } else if let Some(bullet) = line.strip_prefix("- ") {
            finish_item(&mut item, &mut current);
            finish_paragraph(&mut paragraph, &mut current);
            item.push(bullet.to_string());
        } else if line.starts_with("  ") && !item.is_empty() {
            item.push(line.to_string());
        } else if line.trim().is_empty() {
            finish_item(&mut item, &mut current);
            finish_paragraph(&mut paragraph, &mut current);
        } else {
            finish_item(&mut item, &mut current);
            paragraph.push(line.to_string());
        }
    }
    finish_item(&mut item, &mut current);
    finish_paragraph(&mut paragraph, &mut current);
    if let Some(rel) = current.take() {
        out.push(rel);
    }
    out
}

/// Whether there is anything to show, and what.
pub fn since(from: Option<&str>, to: &str) -> Vec<Release> {
    match from {
        // A first run has nothing to compare against, and the setup wizard
        // has just walked the user through the app anyway.
        None => Vec::new(),
        Some(from) => releases_between(CHANGELOG, from, to),
    }
}

// ---------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------

use std::time::{Duration, Instant};

/// How long each entry waits before it appears. Slow enough that the eye
/// follows one line at a time, fast enough that nobody sits through it.
const REVEAL_STEP: Duration = Duration::from_millis(110);

/// One screenful: a release, or part of one when it does not fit.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// Version heading for this screen.
    pub version: String,
    /// Title after the em dash, when there is one.
    pub title: String,
    /// Rendered rows, each tagged with the entry it belongs to so entries
    /// appear whole rather than line by line.
    pub rows: Vec<(usize, String)>,
    /// `(part, of)` when one release needed more than one screen.
    pub part: (usize, usize),
    /// Index, within the release, of the first entry on this screen.
    pub first_item: usize,
    /// Whether this screen is all note and no list.
    pub note: bool,
}

/// The paged "what arrived while you were away" screen.
pub struct WhatsNew {
    releases: Vec<Release>,
    pages: Vec<Page>,
    width: usize,
    height: usize,
    /// Current screen.
    pub page: usize,
    shown_at: Instant,
    /// Set once the user has asked to see this page all at once.
    all_at_once: bool,
    /// Whether entries are shown in full rather than just their opening claim.
    expanded: bool,
    /// Version this screen is announcing.
    to: String,
    /// Continue presses made on the current screen.
    presses: usize,
}

const DIM: &str = "\x1b[38;2;135;130;125m";
const TEXT: &str = "\x1b[38;2;200;195;185m";
const BRIGHT: &str = "\x1b[1;38;2;240;235;225m";
const ACCENT: &str = "\x1b[1;38;2;225;175;95m";
const CODE: &str = "\x1b[38;2;175;170;225m";
const BORDER: &str = "\x1b[38;2;100;95;90m";
const RESET: &str = "\x1b[0m";

/// Colour `code` spans inside a changelog entry.
fn style_inline(text: &str, base: &str) -> String {
    let mut out = String::from(base);
    let mut in_code = false;
    for part in text.split('`') {
        if in_code {
            out.push_str(CODE);
            out.push_str(part);
        } else {
            // Outside code spans the file's own emphasis is markup, and
            // printing "**Pictures.**" on screen shows the marks rather than
            // the emphasis.
            let mut bold = false;
            for chunk in part.split("**") {
                let style = if bold { BRIGHT } else { base };
                out.push_str(style);
                out.push_str(&italicise(chunk, style));
                bold = !bold;
            }
        }
        in_code = !in_code;
    }
    out.push_str(RESET);
    out
}

/// Render `*emphasis*` as italic and drop the asterisks. An asterisk only
/// opens emphasis when text follows it directly and only closes it when text
/// precedes it, so a lone `*` in prose is left alone.
fn italicise(text: &str, style: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '*' && i + 1 < chars.len() && !chars[i + 1].is_whitespace() && chars[i + 1] != '*'
            && (i == 0 || !chars[i - 1].is_alphanumeric())
        {
            if let Some(close) = (i + 2..chars.len()).find(|&j| {
                chars[j] == '*' && !chars[j - 1].is_whitespace() && (j + 1 == chars.len() || !chars[j + 1].is_alphanumeric())
            }) {
                out.push_str("\x1b[3m");
                out.extend(&chars[i + 1..close]);
                out.push_str("\x1b[23m");
                out.push_str(style);
                i = close + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

impl WhatsNew {
    /// Build the screen for `releases`, laid out for a terminal of this size.
    pub fn new(releases: Vec<Release>, to: &str, width: usize, height: usize) -> Self {
        let mut view = Self {
            releases,
            pages: Vec::new(),
            width: 0,
            height: 0,
            page: 0,
            shown_at: Instant::now(),
            all_at_once: false,
            expanded: false,
            to: to.to_string(),
            presses: 0,
        };
        view.relayout(width, height);
        view
    }

    /// Number of screens the user will page through.
    pub fn pages(&self) -> usize {
        self.pages.len()
    }

    /// Re-flow for a new terminal size, keeping the reader roughly where they
    /// were: a resize must not throw away the place in the list.
    pub fn relayout(&mut self, width: usize, height: usize) {
        if self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        self.rebuild();
    }

    /// Re-flow, keeping the reader on the release they were reading: a resize
    /// or a change of detail must not throw away their place in the list.
    fn rebuild(&mut self) {
        // Where the reader is: the release, and the first entry on screen.
        // Keeping only the release sent someone who asked for more detail on
        // the third screen of a long release back to its first.
        let seen = self.pages.get(self.page).map(|p| (p.version.clone(), p.first_item));
        self.pages = layout(&self.releases, self.text_width(), self.body_rows(), self.expanded);
        self.page = seen
            .and_then(|(version, item)| {
                self.pages
                    .iter()
                    .rposition(|p| p.version == version && p.first_item <= item)
                    .or_else(|| self.pages.iter().position(|p| p.version == version))
            })
            .unwrap_or(0)
            .min(self.pages.len().saturating_sub(1));
        self.shown_at = Instant::now();
        // Detail the reader asked for is not something to watch arrive.
        self.all_at_once = self.expanded;
    }

    /// Whether the full text is on screen.
    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    fn inner_width(&self) -> usize {
        self.width.saturating_sub(6).clamp(14, 110)
    }

    fn text_width(&self) -> usize {
        self.inner_width().saturating_sub(6).max(16)
    }

    /// Rows available for the entries themselves, after the frame, the
    /// heading and the footer.
    fn body_rows(&self) -> usize {
        self.height.saturating_sub(9).clamp(3, 24)
    }

    /// Entries revealed so far on this page.
    fn revealed(&self) -> usize {
        if self.all_at_once {
            return usize::MAX;
        }
        1 + (self.shown_at.elapsed().as_millis() / REVEAL_STEP.as_millis()) as usize
    }

    /// Whether the whole page is on screen already.
    pub fn fully_revealed(&self) -> bool {
        let entries = self
            .pages
            .get(self.page)
            .and_then(|p| p.rows.last().map(|(e, _)| e + 1))
            .unwrap_or(0);
        self.revealed() >= entries
    }

    /// Whether the screen still has something to draw on its own — the caller
    /// keeps redrawing while this is true.
    pub fn animating(&self) -> bool {
        !self.fully_revealed()
    }

    fn go(&mut self, page: usize) {
        self.page = page;
        self.shown_at = Instant::now();
        self.all_at_once = false;
        self.presses = 0;
    }

    /// Whether moving on from this screen leaves a note behind. A note that
    /// runs over two screens is only held on the last of them.
    fn leaving_note(&self) -> bool {
        let Some(page) = self.pages.get(self.page) else {
            return false;
        };
        page.note
            && !self
                .pages
                .get(self.page + 1)
                .is_some_and(|next| next.note && next.version == page.version)
    }

    /// Presses still needed before the reader is let past this screen,
    /// counting the one that moves on.
    fn presses_needed(&self) -> usize {
        if self.leaving_note() {
            NOTE_PRESSES.saturating_sub(self.presses).max(1)
        } else {
            1
        }
    }

    /// Count a press that would move past the screen; true while the screen
    /// still holds the reader.
    fn hold(&mut self) -> bool {
        if !self.leaving_note() {
            return false;
        }
        self.presses += 1;
        if self.presses < NOTE_PRESSES {
            self.all_at_once = true;
            return true;
        }
        false
    }

    /// Handle a key. `Some(())` means the screen is finished with.
    pub fn handle_key(&mut self, code: KeyCode) -> Option<()> {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                // Skipping counts as a press, not as a way around the note.
                if self.hold() {
                    return None;
                }
                Some(())
            }
            KeyCode::Tab | KeyCode::Char('m') => {
                // Every entry at full length is a wall; the opening claim
                // alone is sometimes not enough. Both, on one key.
                self.expanded = !self.expanded;
                self.rebuild();
                None
            }
            KeyCode::Left | KeyCode::Backspace | KeyCode::PageUp | KeyCode::Char('k') => {
                if self.page > 0 {
                    let prev = self.page - 1;
                    self.go(prev);
                    // Going back is going back to read, not to watch it
                    // appear again.
                    self.all_at_once = true;
                }
                None
            }
            KeyCode::Enter
            | KeyCode::Right
            | KeyCode::Char(' ')
            | KeyCode::PageDown
            | KeyCode::Char('j') => {
                if self.hold() {
                    return None;
                }
                if !self.fully_revealed() {
                    // Impatience is an instruction: show the rest now.
                    self.all_at_once = true;
                    return None;
                }
                if self.page + 1 < self.pages.len() {
                    let next = self.page + 1;
                    self.go(next);
                    None
                } else {
                    Some(())
                }
            }
            _ => None,
        }
    }

    /// Draw the screen.
    pub fn render(&self) -> Vec<String> {
        let inner_w = self.inner_width();
        let mut lines = Vec::new();

        // The heading is the first thing that stops fitting in a narrow
        // window, and a sheared-open box is worse than a shorter name.
        let title = [
            format!(" What's new in FlashAgent {} ", self.to),
            format!(" What's new · {} ", self.to),
            format!(" {} ", self.to),
        ]
        .into_iter()
        // One dash of frame always stays to the right of the heading.
        .find(|t| crate::visible_width(t) < inner_w)
        .unwrap_or_else(|| " new ".to_string());
        let dashes = inner_w.saturating_sub(crate::visible_width(&title) + 1);
        lines.push(format!(
            "  {BORDER}┌─{ACCENT}{title}{BORDER}{}┐{RESET}",
            "─".repeat(dashes)
        ));

        let pad = |content: &str| -> String {
            let max_w = inner_w.saturating_sub(2);
            let vis = crate::visible_width(content);
            let clipped = if vis > max_w { crate::clip_ansi(content, max_w) } else { content.to_string() };
            let pad = " ".repeat(max_w.saturating_sub(crate::visible_width(&clipped)));
            format!("  {BORDER}│{RESET} {clipped}{pad} {BORDER}│{RESET}")
        };

        let Some(page) = self.pages.get(self.page) else {
            lines.push(pad(""));
            lines.push(format!("  {BORDER}└{}┘{RESET}", "─".repeat(inner_w)));
            return lines;
        };

        lines.push(pad(""));
        let mut heading = format!("{ACCENT}{}{RESET}", page.version);
        if !page.title.is_empty() {
            heading.push_str(&format!("{DIM} — {BRIGHT}{}{RESET}", page.title));
        }
        if page.part.1 > 1 {
            heading.push_str(&format!(" {DIM}({} of {}){RESET}", page.part.0, page.part.1));
        }
        lines.push(pad(&heading));
        lines.push(pad(""));

        let revealed = self.revealed();
        let mut body = 0;
        for (entry, row) in &page.rows {
            if *entry >= revealed {
                break;
            }
            lines.push(pad(row));
            body += 1;
        }
        // Hold the box still while the entries arrive, so the footer does not
        // crawl down the screen.
        for _ in body..page.rows.len() {
            lines.push(pad(""));
        }

        lines.push(pad(""));
        let counter = if self.pages.len() > 1 {
            format!("{DIM}{}/{}{RESET}   ", self.page + 1, self.pages.len())
        } else {
            String::new()
        };
        let detail = if self.expanded { "tab — less" } else { "tab — more" };
        let back = if self.page > 0 { " · ← — back" } else { "" };
        let needed = self.presses_needed();
        let hint = if needed > 1 {
            format!("press enter {needed} times to go on{back}")
        } else if !self.fully_revealed() {
            format!("enter — show all · {detail} · esc — skip")
        } else if self.page + 1 < self.pages.len() {
            format!("enter — next · ← — back · {detail} · esc — skip")
        } else if self.pages.len() > 1 {
            format!("enter — start working · ← — back · {detail}")
        } else {
            format!("enter — start working · {detail}")
        };
        lines.push(pad(&format!("{counter}{DIM}{hint}{RESET}")));
        lines.push(format!("  {BORDER}└{}┘{RESET}", "─".repeat(inner_w)));
        lines
    }
}

/// Break the releases into screens of at most `rows` body rows.
/// The claim a changelog entry opens with, and the detail after it.
///
/// Entries are written to be read in full in the file; on a screen the first
/// sentence is what someone actually wants — the rest is there for whoever
/// asks for it.
fn split_claim(item: &str) -> (String, String) {
    let bytes = item.as_bytes();
    let mut depth_code = false;
    for (i, c) in item.char_indices() {
        if c == '`' {
            depth_code = !depth_code;
        }
        if depth_code || !matches!(c, '.' | '!' | '?') {
            continue;
        }
        let next = bytes.get(i + 1).copied();
        // A sentence ends on punctuation followed by a space, not on the dot
        // inside `v1.0.0` or `main.rs`.
        if matches!(next, Some(b' ') | None) {
            let head = item[..=i].trim().to_string();
            // A three-word opener is a fragment, not a claim.
            if head.split_whitespace().count() >= 5 {
                return (head, item[i + 1..].trim().to_string());
            }
        }
    }
    (item.to_string(), String::new())
}

/// An entry's rendered rows, with whether it asks for a new screen and whether
/// it is prose.
type Entry = (Vec<String>, (bool, bool));

fn layout(releases: &[Release], text_w: usize, rows: usize, expanded: bool) -> Vec<Page> {
    let mut pages = Vec::new();
    for rel in releases {
        // Each entry becomes one or more wrapped rows, indented under its
        // marker so a wrapped line still reads as part of the same point.
        let mut entries: Vec<Vec<String>> = Vec::new();
        // Per entry: whether it asks for a new screen, and whether it is prose.
        let mut kinds: Vec<(bool, bool)> = Vec::new();
        for item in &rel.items {
            if item == PAGE {
                entries.push(Vec::new());
                kinds.push((true, false));
                continue;
            }
            kinds.push((false, item.starts_with(PROSE)));
            let rendered = if let Some(head) = item.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                // A heading after a list needs air above it; after a
                // paragraph, which already ends in a blank row, it does not.
                let after_blank = entries.last().is_none_or(|e: &Vec<String>| e.last().is_some_and(|r| r.is_empty()));
                if after_blank {
                    vec![format!("  {BRIGHT}{head}{RESET}")]
                } else {
                    vec![String::new(), format!("  {BRIGHT}{head}{RESET}")]
                }
            } else if let Some(text) = item.strip_prefix(PROSE) {
                // A letter is not a list: no marker, no folding to the first
                // sentence, and a blank row after each paragraph.
                let mut rows: Vec<String> = crate::wrap_styled(&style_inline(text, TEXT), text_w + 2)
                    .into_iter()
                    .map(|row| format!("  {row}"))
                    .collect();
                rows.push(String::new());
                rows
            } else {
                let (claim, detail) = split_claim(item);
                let shown = if expanded || detail.is_empty() {
                    style_inline(item, TEXT)
                } else {
                    // The claim in full, and a mark saying there is more.
                    format!("{}{DIM} …{RESET}", style_inline(&claim, TEXT))
                };
                crate::wrap_styled(&shown, text_w)
                    .into_iter()
                    .enumerate()
                    .map(|(i, row)| {
                        if i == 0 {
                            format!("  {ACCENT}›{RESET} {row}")
                        } else {
                            // Under the text after "› ", not under the marker.
                            format!("    {row}")
                        }
                    })
                    .collect()
            };
            entries.push(rendered);
        }

        let mut chunks: Vec<Vec<Entry>> = Vec::new();
        let mut chunk: Vec<Entry> = Vec::new();
        let mut used = 0;
        for (entry, kind) in entries.into_iter().zip(kinds) {
            let len = entry.len();
            let (page_mark, _) = kind;
            if !chunk.is_empty() && (page_mark || used + len > rows) {
                chunks.push(std::mem::take(&mut chunk));
                used = 0;
            }
            used += len;
            chunk.push((entry, kind));
        }
        if !chunk.is_empty() {
            chunks.push(chunk);
        }

        let total = chunks.len();
        let mut first_item = 0;
        for (i, chunk) in chunks.into_iter().enumerate() {
            let count = chunk.len();
            let note = chunk.iter().any(|(_, (_, prose))| *prose)
                && chunk.iter().all(|(_, (page_mark, prose))| *page_mark || *prose);
            let mut page_rows = Vec::new();
            for (entry_idx, (entry, _)) in chunk.into_iter().enumerate() {
                for row in entry {
                    // A screen does not open on the blank row meant to
                    // separate a heading from what came before it.
                    if page_rows.is_empty() && row.is_empty() {
                        continue;
                    }
                    page_rows.push((entry_idx, row));
                }
            }
            // Nor does it end on one: the blank after a closing paragraph is
            // a row the next screen's content could have used.
            while page_rows.last().is_some_and(|(_, row)| row.is_empty()) {
                page_rows.pop();
            }
            pages.push(Page {
                version: rel.version.clone(),
                title: rel.title.clone(),
                rows: page_rows,
                part: (i + 1, total),
                first_item,
                note,
            });
            first_item += count;
        }
    }
    pages
}

/// Show the screen from inside the running app, where the event reader
/// already owns stdin and raw mode must stay on.
pub async fn run_channel(
    releases: Vec<Release>,
    to: &str,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::UiEvent>,
) -> std::io::Result<()> {
    use crossterm::{cursor, execute, terminal::{EnterAlternateScreen, LeaveAlternateScreen}};
    use std::io::Write;

    if releases.is_empty() {
        return Ok(());
    }

    let mut stdout = std::io::stdout();
    let _ = execute!(stdout, EnterAlternateScreen, cursor::Hide);

    let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
    let mut view = WhatsNew::new(releases, to, w as usize, h as usize);

    loop {
        let mut buf = String::from("\x1b[H\x1b[2J\r\n");
        for line in view.render() {
            buf.push_str(&line);
            buf.push_str("\r\n");
        }
        let _ = stdout.write_all(buf.as_bytes());
        let _ = stdout.flush();

        // While entries are still arriving the screen has to redraw on its
        // own; once it is whole, waiting on a key costs nothing.
        let next = tokio::time::timeout(Duration::from_millis(40), rx.recv()).await;
        match next {
            Ok(Some(crate::UiEvent::Key(code, _))) => {
                if view.handle_key(code).is_some() {
                    break;
                }
            }
            Ok(Some(crate::UiEvent::Resize(w, h))) => view.relayout(w as usize, h as usize),
            Ok(None) => break,
            _ => {}
        }
    }

    let _ = execute!(stdout, cursor::Show, LeaveAlternateScreen);
    Ok(())
}

/// Show the screen, then return. Takes over the terminal for as long as it is
/// up and hands it back exactly as it was found.
pub async fn run(releases: Vec<Release>, to: &str) -> std::io::Result<()> {
    use crossterm::{
        cursor, execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };
    use std::io::Write;

    if releases.is_empty() {
        return Ok(());
    }

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    let _ = execute!(stdout, EnterAlternateScreen, cursor::Hide);

    let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
    let mut view = WhatsNew::new(releases, to, w as usize, h as usize);

    loop {
        let buffer = {
            let mut buf = String::from("\x1b[H\x1b[2J\r\n");
            for line in view.render() {
                buf.push_str(&line);
                buf.push_str("\r\n");
            }
            buf
        };
        let _ = stdout.write_all(buffer.as_bytes());
        let _ = stdout.flush();

        if crossterm::event::poll(Duration::from_millis(40)).unwrap_or(false) {
            match crossterm::event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if view.handle_key(key.code).is_some() {
                        break;
                    }
                }
                Event::Resize(w, h) => view.relayout(w as usize, h as usize),
                _ => {}
            }
        }
    }

    let _ = execute!(stdout, cursor::Show, LeaveAlternateScreen);
    let _ = disable_raw_mode();
    Ok(())
}

/// The newest `n` releases in the bundled changelog, for when the screen is
/// asked for rather than triggered.
pub fn latest(n: usize) -> Vec<Release> {
    let mut all = releases_between(CHANGELOG, "b0", "v9999.0.0");
    all.truncate(n);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Changelog\n\
        \n\
        Some preamble.\n\
        \n\
        ## Unreleased\n\
        \n\
        - not shipped yet\n\
        \n\
        ## b238 — no emoji in the tool cards\n\
        \n\
        - Tool cards drop their emoji icons. `Edited notes.md`, and the\n  \
          extension already says what a file is.\n\
        - Directories still end in `/`.\n\
        \n\
        ## b236 — updates you can watch\n\
        \n\
        - Ctrl+U shows the download.\n\
        \n\
        ## b233 — end-to-end audit\n\
        \n\
        ### Tool calling\n\
        - Text tool calls are executed.\n";

    fn sample_releases() -> Vec<Release> {
        releases_between(SAMPLE, "b218", "b238")
    }

    #[test]
    fn a_page_opens_with_the_claims_not_the_whole_text() {
        // Five paragraphs at once is a wall nobody reads; the first sentence
        // of each is what the screen is for.
        let releases = releases_between(CHANGELOG, "b238", "b245");
        let short = WhatsNew::new(releases.clone(), "b245", 100, 40);
        let long = {
            let mut v = WhatsNew::new(releases, "b245", 100, 40);
            v.handle_key(KeyCode::Tab);
            v
        };
        assert!(long.is_expanded());
        let rows_short: usize = short.pages.iter().map(|p| p.rows.len()).sum();
        let rows_long: usize = long.pages.iter().map(|p| p.rows.len()).sum();
        assert!(rows_short * 2 < rows_long, "short {rows_short} vs full {rows_long}");
        let dump = crate::strip_ansi(&short.render().join("\n"));
        assert!(dump.contains('…'), "the mark saying there is more is missing:\n{dump}");
        assert!(dump.contains("tab — more"), "{dump}");
    }

    #[test]
    fn asking_for_detail_keeps_you_on_the_release_you_were_reading() {
        let mut view = WhatsNew::new(releases_between(CHANGELOG, "b233", "b245"), "b245", 100, 24);
        view.handle_key(KeyCode::Enter);
        view.handle_key(KeyCode::Enter);
        let version = view.pages[view.page].version.clone();
        view.handle_key(KeyCode::Tab);
        assert_eq!(view.pages[view.page].version, version);
        assert!(view.fully_revealed(), "detail you asked for does not animate in");
    }

    const LETTER: &str = "# Changelog\n\n\
        ## b260 — a note first\n\n\
        A lot of people think vibecoded means slop. This release is an answer to\n\
        that, on this exact project.\n\n\
        Second paragraph, with `code` in it.\n\n\
        — flashback\n\n\
        ### What changed\n\n\
        - Removed the banner.\n\
        - Measured the numbers.\n";

    #[test]
    fn a_release_can_open_with_prose_before_its_list() {
        let rels = releases_between(LETTER, "b250", "b260");
        let items = &rels[0].items;
        assert_eq!(items.len(), 6, "{items:?}");
        assert!(items[0].starts_with(PROSE));
        assert!(items[0].contains("This release is an answer to that, on this exact project."), "wrapped lines rejoin: {items:?}");
        assert!(items[2].ends_with("flashback"), "the sign-off is its own paragraph");
        assert_eq!(items[3], "[What changed]");
        assert_eq!(items[4], "Removed the banner.", "bullets after the letter are unchanged");
    }

    #[test]
    fn prose_is_shown_whole_without_a_bullet_marker() {
        let view = WhatsNew::new(releases_between(LETTER, "b250", "b260"), "b260", 100, 40);
        let rows: Vec<String> = view.pages[0].rows.iter().map(|(_, r)| crate::strip_ansi(r)).collect();
        let first = rows.iter().position(|r| r.contains("A lot of people")).expect("prose on the page");
        assert!(!rows[first].contains('›'), "prose has no marker: {:?}", rows[first]);
        let joined = rows.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(joined.contains("on this exact project."), "not folded to its first sentence: {joined}");
        assert!(!joined.contains('\u{b6}'), "the internal marker never reaches the screen");
        assert!(joined.contains("› Removed the banner."), "listed changes keep their marker");
    }

    #[test]
    fn a_long_letter_spans_pages() {
        let long_para = "word ".repeat(120);
        let log = format!("# Changelog\n\n## b260 — long\n\n{long_para}\n\n{long_para}\n\n{long_para}\n\n- one change\n");
        let view = WhatsNew::new(releases_between(&log, "b250", "b260"), "b260", 80, 20);
        assert!(view.pages() >= 2, "a letter taller than the window spans screens, got {}", view.pages());
    }

    fn long_release() -> Vec<Release> {
        let mut items = vec![format!("{PROSE}{}", "opening words ".repeat(30))];
        for i in 0..14 {
            items.push(format!("Change number {i} does one thing. And then it explains the thing at some length, enough to fold."));
        }
        items.push("[Later]".to_string());
        items.push("A change under the later heading.".to_string());
        vec![Release { version: "b260".into(), title: "long".into(), items }]
    }

    #[test]
    fn asking_for_detail_keeps_the_reader_on_the_screen_they_were_reading() {
        let mut view = WhatsNew::new(long_release(), "b260", 80, 20);
        assert!(view.pages() >= 3, "needs several screens, got {}", view.pages());
        view.handle_key(KeyCode::Enter);
        view.handle_key(KeyCode::Enter);
        let reading = view.pages[view.page].first_item;
        assert!(reading > 0, "moved off the first screen");
        view.handle_key(KeyCode::Tab);
        let now = &view.pages[view.page];
        let next_first = view.pages.get(view.page + 1).map_or(usize::MAX, |p| p.first_item);
        assert!(now.first_item <= reading && reading < next_first, "entry {reading} is not on screen {} ({}..{next_first})", view.page, now.first_item);
        assert!(view.page > 0, "expanding did not send the reader back to the start");
    }

    #[test]
    fn a_heading_gets_air_above_it_but_a_screen_never_opens_on_a_blank_row() {
        let view = WhatsNew::new(long_release(), "b260", 80, 40);
        for page in &view.pages {
            assert!(!page.rows.first().is_some_and(|(_, r)| r.is_empty()), "screen {:?} opens on a blank row", page.part);
        }
        let all: Vec<String> = view.pages.iter().flat_map(|p| p.rows.iter().map(|(_, r)| crate::strip_ansi(r))).collect();
        let at = all.iter().position(|r| r.trim() == "Later").expect("heading rendered");
        if at > 0 && !view.pages.iter().any(|p| p.rows.first().is_some_and(|(_, r)| crate::strip_ansi(r).trim() == "Later")) {
            assert!(all[at - 1].is_empty(), "no blank row before the heading: {:?}", &all[at.saturating_sub(2)..=at]);
        }
    }

    const PAGED: &str = "# Changelog\n\n\
        ## b260 — paged\n\n\
        A short note.\n\n\
        — flashback\n\n\
        <!-- page -->\n\n\
        ### One\n\n\
        - The first change.\n\n\
        <!-- page -->\n\n\
        ### Two\n\n\
        - The second change.\n";

    fn plain_rows(view: &WhatsNew) -> String {
        view.pages.iter().flat_map(|p| p.rows.iter().map(|(_, r)| crate::strip_ansi(r))).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn a_page_mark_starts_a_new_screen_and_is_never_shown() {
        let view = WhatsNew::new(releases_between(PAGED, "b250", "b260"), "b260", 100, 40);
        assert_eq!(view.pages(), 3, "{:#?}", view.pages);
        let text = plain_rows(&view);
        assert!(!text.contains("<!--") && !text.contains('\u{c}'), "{text}");
        assert!(view.pages[0].note, "the first screen is the note");
        assert!(!view.pages[1].note && !view.pages[2].note);
        assert!(crate::strip_ansi(&view.render()[2]).contains("1 of 3"));
    }

    #[test]
    fn the_note_takes_three_presses_to_leave() {
        let mut view = WhatsNew::new(releases_between(PAGED, "b250", "b260"), "b260", 100, 40);
        assert!(crate::strip_ansi(&view.render().join("\n")).contains("press enter 3 times"));
        assert_eq!(view.handle_key(KeyCode::Enter), None);
        assert_eq!(view.handle_key(KeyCode::Char(' ')), None);
        assert_eq!(view.page, 0, "two presses are not enough");
        assert!(view.fully_revealed(), "the note is on screen in full while it waits");
        view.handle_key(KeyCode::Right);
        assert_eq!(view.page, 1, "any continue key counts, and the third one moves on");
        // The list after it pages as it always did.
        view.handle_key(KeyCode::Enter);
        view.handle_key(KeyCode::Enter);
        assert_eq!(view.page, 2);
    }

    #[test]
    fn esc_on_the_note_counts_as_a_press_rather_than_a_way_around_it() {
        let mut view = WhatsNew::new(releases_between(PAGED, "b250", "b260"), "b260", 100, 40);
        assert_eq!(view.handle_key(KeyCode::Esc), None);
        assert_eq!(view.handle_key(KeyCode::Esc), None);
        assert_eq!(view.handle_key(KeyCode::Esc), Some(()));
    }

    #[test]
    fn coming_back_to_the_note_asks_again_and_leaving_the_list_does_not() {
        let mut view = WhatsNew::new(releases_between(PAGED, "b250", "b260"), "b260", 100, 40);
        for _ in 0..3 {
            view.handle_key(KeyCode::Enter);
        }
        assert_eq!(view.page, 1);
        assert_eq!(view.handle_key(KeyCode::Esc), Some(()), "only the note holds");
        view.handle_key(KeyCode::Left);
        assert_eq!(view.page, 0);
        view.handle_key(KeyCode::Enter);
        assert_eq!(view.page, 0, "the count starts over");
    }

    #[test]
    fn a_note_taller_than_the_window_is_held_only_on_its_last_screen() {
        let para = "word ".repeat(120);
        let log = format!("# Changelog\n\n## b260 — long\n\n{para}\n\n{para}\n\n{para}\n\n<!-- page -->\n\n- one change\n");
        let mut view = WhatsNew::new(releases_between(&log, "b250", "b260"), "b260", 80, 20);
        let notes = view.pages.iter().take_while(|p| p.note).count();
        assert!(notes >= 2, "the note spans screens: {}", view.pages());
        // Every screen shown whole first, so each press below is a page turn.
        while view.page + 1 < notes {
            let before = view.page;
            view.all_at_once = true;
            view.handle_key(KeyCode::Enter);
            assert_eq!(view.page, before + 1, "the early part of a note pages with one press");
        }
        let last = view.page;
        view.all_at_once = true;
        view.handle_key(KeyCode::Enter);
        view.handle_key(KeyCode::Enter);
        assert_eq!(view.page, last, "its last screen holds");
        view.handle_key(KeyCode::Enter);
        assert_eq!(view.page, last + 1);
    }

    #[test]
    fn a_screen_does_not_end_on_a_blank_row() {
        let view = WhatsNew::new(releases_between(PAGED, "b250", "b260"), "b260", 100, 40);
        for page in &view.pages {
            assert!(!page.rows.last().is_some_and(|(_, r)| r.is_empty()), "{:?}", page.part);
        }
    }

    #[test]
    fn the_b260_notes_are_a_letter_and_four_screens() {
        let view = WhatsNew::new(releases_between(CHANGELOG, "b250", "b260"), "b260", 120, 40);
        let sizes: Vec<usize> = view.pages.iter().map(|p| p.rows.len()).collect();
        assert_eq!(view.pages(), 5, "rows per screen: {sizes:?}\n{}", plain_rows(&view));
        assert!(view.pages[0].note);
        assert!(view.pages[1..].iter().all(|p| !p.note));
    }

    #[test]
    fn single_asterisk_emphasis_is_italic_and_the_marks_are_gone() {
        let styled = style_inline("it reads *Thought: … (4s)* now, and 2 * 3 stays", TEXT);
        assert_eq!(crate::strip_ansi(&styled), "it reads Thought: … (4s) now, and 2 * 3 stays");
        assert!(styled.contains("\x1b[3m"), "the emphasis is kept, as italic");
        let code = style_inline("tools like `get_*` stay literal", TEXT);
        assert_eq!(crate::strip_ansi(&code), "tools like get_* stay literal");
    }

    #[test]
    fn wrapped_rows_line_up_under_their_text() {
        let long = "Approval cards name files relative to the project and the preview rows go to the change itself instead of repeating the file name".to_string();
        let rel = vec![Release { version: "b260".into(), title: String::new(), items: vec![long, format!("{PROSE}{}", "word ".repeat(40))] }];
        let mut view = WhatsNew::new(rel, "b260", 60, 40);
        view.handle_key(KeyCode::Tab);
        let rows: Vec<String> = view.pages[0].rows.iter().map(|(_, r)| crate::strip_ansi(r)).collect();
        let bullet: Vec<&String> = rows.iter().take_while(|r| !r.trim().starts_with("word")).filter(|r| !r.is_empty()).collect();
        assert!(bullet.len() > 1, "{rows:?}");
        // Columns, not bytes: the marker is one column but three bytes.
        let text_col = bullet[0][..bullet[0].find("Approval").unwrap()].chars().count();
        for r in &bullet[1..] {
            assert_eq!(r.len() - r.trim_start().len(), text_col, "continuation not under the text: {r:?}");
        }
        let prose: Vec<&String> = rows.iter().filter(|r| r.trim().starts_with("word")).collect();
        assert!(prose.len() > 1);
        assert!(prose.iter().all(|r| r.len() - r.trim_start().len() == 2), "prose rows share one indent: {prose:?}");
    }

    #[test]
    fn the_changelogs_own_markup_is_shown_as_emphasis_not_as_asterisks() {
        let styled = style_inline("**Pictures.** `Ctrl+V` pastes one", TEXT);
        let plain = crate::strip_ansi(&styled);
        assert_eq!(plain, "Pictures. Ctrl+V pastes one");
        assert!(styled.contains(BRIGHT), "the emphasis is kept, as colour");
        assert!(styled.contains(CODE));
    }

    #[test]
    fn a_claim_is_a_whole_sentence_and_a_version_number_is_not_the_end_of_one() {
        let (claim, rest) = split_claim("Betas now rank below v1.0.0 in the changelog. That fixes the jump.");
        assert_eq!(claim, "Betas now rank below v1.0.0 in the changelog.");
        assert_eq!(rest, "That fixes the jump.");

        let (whole, nothing) = split_claim("Directories still end in `/`.");
        assert_eq!(whole, "Directories still end in `/`.");
        assert_eq!(nothing, "", "a one-sentence entry has no hidden half");

        let (short, tail) = split_claim("Fixed. The recap request was capped at 512 tokens and ran out.");
        assert!(short.starts_with("Fixed."), "{short}");
        assert!(tail.is_empty(), "a two-word opener is a fragment, not a claim");
    }

    #[test]
    fn every_row_fits_the_terminal_it_was_drawn_for() {
        // The screen takes over the whole terminal; a row wider than the
        // window wraps and shears the box open.
        for width in [30usize, 44, 60, 80, 120] {
            let view = WhatsNew::new(sample_releases(), "b238", width, 24);
            for row in view.render() {
                assert!(
                    crate::visible_width(&row) <= width,
                    "{width} columns: {:?} is {} wide",
                    crate::strip_ansi(&row),
                    crate::visible_width(&row)
                );
            }
        }
    }

    #[test]
    fn entries_arrive_one_at_a_time_and_the_box_does_not_move() {
        let view = WhatsNew::new(sample_releases(), "b238", 80, 24);
        assert!(!view.fully_revealed(), "the first frame shows one entry, not all of them");
        let early = view.render();

        let mut done = WhatsNew::new(sample_releases(), "b238", 80, 24);
        done.handle_key(KeyCode::Enter);
        assert!(done.fully_revealed(), "enter shows the rest at once");
        assert_eq!(
            early.len(),
            done.render().len(),
            "the frame is the same height throughout, so the footer stays put"
        );
        assert!(
            crate::strip_ansi(&done.render().join("\n")).contains("Tool cards drop their emoji"),
            "the entries are actually on the screen"
        );
    }

    #[test]
    fn enter_walks_the_releases_and_then_finishes() {
        let mut view = WhatsNew::new(sample_releases(), "b238", 80, 24);
        let pages = view.pages();
        assert!(pages >= 2, "the sample has more than one release");
        for page in 0..pages {
            assert_eq!(view.page, page);
            view.handle_key(KeyCode::Enter); // reveal
            assert_eq!(view.handle_key(KeyCode::Enter), if page + 1 == pages { Some(()) } else { None });
        }
        assert_eq!(view.page, pages - 1);
    }

    #[test]
    fn going_back_shows_the_page_whole_rather_than_replaying_it() {
        let mut view = WhatsNew::new(sample_releases(), "b238", 80, 24);
        view.handle_key(KeyCode::Enter);
        view.handle_key(KeyCode::Enter);
        assert_eq!(view.page, 1);
        view.handle_key(KeyCode::Left);
        assert_eq!(view.page, 0);
        assert!(view.fully_revealed(), "a page you have already read does not animate again");
    }

    #[test]
    fn esc_leaves_from_anywhere() {
        let mut view = WhatsNew::new(sample_releases(), "b238", 80, 24);
        assert_eq!(view.handle_key(KeyCode::Esc), Some(()));
    }

    #[test]
    fn a_short_window_splits_one_release_across_screens() {
        let long = vec![Release {
            version: "b240".into(),
            title: "many things".into(),
            items: (0..12).map(|i| format!("entry number {i}")).collect(),
        }];
        let tall = WhatsNew::new(long.clone(), "b240", 80, 40);
        let short = WhatsNew::new(long, "b240", 80, 14);
        assert!(short.pages() > tall.pages(), "{} vs {}", short.pages(), tall.pages());
        let head = crate::strip_ansi(&short.render()[2]);
        assert!(head.contains("1 of"), "a split release says which part you are on: {head:?}");
    }

    #[test]
    fn a_resize_keeps_the_reader_on_the_release_they_were_reading() {
        let mut view = WhatsNew::new(sample_releases(), "b238", 80, 40);
        view.handle_key(KeyCode::Enter);
        view.handle_key(KeyCode::Enter);
        let version = view.pages[view.page].version.clone();
        view.relayout(50, 20);
        assert_eq!(view.pages[view.page].version, version);
    }

    #[test]
    fn only_releases_between_the_two_builds_are_shown() {
        let rels = releases_between(SAMPLE, "b233", "b238");
        assert_eq!(
            rels.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(),
            vec!["b238", "b236"],
            "the build you were on is not news, and Unreleased does not exist yet"
        );
        assert_eq!(rels[0].title, "no emoji in the tool cards");
    }

    #[test]
    fn a_wrapped_entry_comes_back_as_one_paragraph() {
        let rels = releases_between(SAMPLE, "b236", "b238");
        assert_eq!(rels.len(), 1);
        assert_eq!(
            rels[0].items[0],
            "Tool cards drop their emoji icons. `Edited notes.md`, and the extension already says what a file is."
        );
        assert_eq!(rels[0].items[1], "Directories still end in `/`.");
    }

    #[test]
    fn sub_headings_survive_as_their_own_entry() {
        let rels = releases_between(SAMPLE, "b218", "b233");
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].items, vec!["[Tool calling]", "Text tool calls are executed."]);
    }

    #[test]
    fn the_newest_release_stands_in_when_no_version_was_ever_recorded() {
        // Everyone updating INTO the first build that records a version has
        // nothing recorded, and they are precisely the people with news to
        // read. `latest` is what they get instead of silence.
        let newest = latest(1);
        assert_eq!(newest.len(), 1);
        assert!(!newest[0].items.is_empty());
        assert!(!newest[0].version.contains("Unreleased"));
        assert_eq!(latest(3).len(), 3);
    }

    #[test]
    fn nothing_is_shown_when_the_build_did_not_move_forward() {
        assert!(releases_between(SAMPLE, "b238", "b238").is_empty());
        assert!(releases_between(SAMPLE, "b238", "b236").is_empty(), "a downgrade is not news");
        assert!(releases_between(SAMPLE, "nonsense", "b238").is_empty());
        assert!(since(None, "b238").is_empty(), "a first run has nothing to compare against");
    }

    #[test]
    fn the_shipped_changelog_parses() {
        // The screen is only as good as this file; a format change here must
        // not quietly empty it.
        let rels = releases_between(CHANGELOG, "b233", "b238");
        assert!(!rels.is_empty(), "the real changelog produced nothing");
        assert!(rels.iter().all(|r| !r.items.is_empty()), "{rels:?}");
        assert!(rels.iter().all(|r| !r.version.contains("Unreleased")));
    }
}
