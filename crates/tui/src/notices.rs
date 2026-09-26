use crate::App;
#[derive(Debug, Clone)]
/// A channel switch replaces the binary with another line of builds, which can
/// remove features and leave settings an older build does not understand, so
/// it is confirmed once, in those words.
pub(crate) struct ChannelSwitch {
    pub(crate) from: flashagent_core::config::UpdateChannel,
    pub(crate) to: flashagent_core::config::UpdateChannel,
    /// Filled in when the check answers.
    pub(crate) target: ChannelTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ChannelTarget {
    Checking,
    Version(String),
    /// The channel exists but has nothing published.
    Empty,
    /// The feed could not be reached; the switch is still the user's call.
    Unknown,
}

pub(crate) fn channel_switch_warning(
    to: flashagent_core::config::UpdateChannel,
    current: &str,
    target: &ChannelTarget,
) -> String {
    use flashagent_svc::Version;
    let channel = to.label().to_lowercase();
    let destination = match target {
        ChannelTarget::Version(v) => v.clone(),
        ChannelTarget::Empty => {
            return format!(
                "Nothing is published on the {channel} channel yet, so you would stay on \
                 {current} until something is. Switch anyway?"
            )
        }
        // Name the channel rather than invent a version or guess the direction.
        ChannelTarget::Checking | ChannelTarget::Unknown => {
            return format!(
                "You will move from {current} to the newest {channel} release once it is found. \
                 Whether that is an update or a downgrade is shown here as soon as it is known. Continue?"
            )
        }
    };
    // Update or downgrade is decided by versions, the same rule as the updater,
    // not by which channel is picked.
    match (Version::parse(current), Version::parse(&destination)) {
        (Some(from), Some(to_version)) if to_version < from => format!(
            "Downgrade: {current} → {destination}. {destination} is older than what you run, so features \
             added since may disappear or behave differently, and settings they introduced can be reset. Continue?"
        ),
        (Some(from), Some(to_version)) if to_version == from => format!(
            "You already run {current}; the {channel} channel has the same release. Switch anyway?"
        ),
        (Some(_), Some(_)) => format!(
            "Update: {current} → {destination}. You get everything added since {current}.{} Continue?",
            if Version::parse(&destination).is_some_and(|v| v.is_beta()) {
                " Beta builds land often and can regress; that is the point of them."
            } else {
                ""
            }
        ),
        _ => format!("You will move from {current} to {destination}. Continue?"),
    }
}

/// Only states the loop actually reported. Nothing is inferred from elapsed
/// time: "almost done" is not something the program knows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TurnPhase {
    /// Request sent, nothing back yet.
    Waiting,
    Thinking,
    Writing,
    /// What it does is on the transcript line above, so the composer does not
    /// repeat it.
    Tool,
    /// The model has the result but has not spoken yet.
    AfterTool,
    Stopping,
}

impl TurnPhase {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Waiting => "Waiting for the model".to_string(),
            Self::Thinking => "Thinking".to_string(),
            Self::Writing => "Writing the answer".to_string(),
            Self::Tool => "Running a tool".to_string(),
            Self::AfterTool => "Reading the result".to_string(),
            Self::Stopping => "Stopping".to_string(),
        }
    }
}


/// Shown under the input. Most fade, so a notice no longer actionable does not
/// hold the line for the session.
pub(crate) struct BackgroundNotice {
    pub(crate) text: String,
    pub(crate) expires: Option<std::time::Instant>,
    /// It fades in rather than popping onto the line.
    pub(crate) born: std::time::Instant,
    /// Something is wrong (no server, a failed update): amber, not green.
    pub(crate) warning: bool,
}

const NOTICE_FADE_IN_MS: f32 = 350.0;

/// A notice about to expire dims over its last second instead of blinking out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct NoticeStyle {
    pub(crate) fade: f32,
    pub(crate) warning: bool,
}

impl NoticeStyle {
    pub(crate) const FULL: Self = Self { fade: 1.0, warning: false };

    pub(crate) fn paint(self, text: &str) -> String {
        let lerp = |from: f32, to: f32| (to + (from - to) * self.fade).round() as u8;
        // From the healthy green, or the warning amber, towards the muted hint grey.
        let (r0, g0, b0) = if self.warning { (235.0, 175.0, 90.0) } else { (120.0, 220.0, 140.0) };
        let (r, g, b) = (lerp(r0, 100.0), lerp(g0, 95.0), lerp(b0, 90.0));
        let bold = if self.fade > 0.6 { "1;" } else { "" };
        format!("\x1b[{bold}38;2;{r};{g};{b}m{text}\x1b[0m")
    }
}

impl BackgroundNotice {
    pub(crate) fn style(&self) -> NoticeStyle {
        let fade_in = if flashagent_tui::anim::enabled() {
            flashagent_tui::anim::ease_out(self.born.elapsed().as_millis() as f32 / NOTICE_FADE_IN_MS)
        } else {
            1.0
        };
        let fade_out = match self.expires {
            None => 1.0,
            Some(at) => at.saturating_duration_since(std::time::Instant::now()).as_secs_f32().clamp(0.0, 1.0),
        };
        NoticeStyle { fade: fade_in.min(fade_out), warning: self.warning }
    }

    pub(crate) fn warning(mut self) -> Self {
        self.warning = true;
        self
    }

    /// Stays until replaced: for anything the user still has to act on.
    pub(crate) fn sticky(text: impl Into<String>) -> Self {
        Self { text: text.into(), expires: None, born: std::time::Instant::now(), warning: false }
    }

    pub(crate) fn fading(text: impl Into<String>, secs: u64) -> Self {
        Self {
            text: text.into(),
            expires: Some(std::time::Instant::now() + std::time::Duration::from_secs(secs)),
            born: std::time::Instant::now(),
            warning: false,
        }
    }

    pub(crate) fn expired(&self) -> bool {
        self.expires.is_some_and(|t| std::time::Instant::now() >= t)
    }
    /// A notice that keeps changing (a download's progress) changes its words in
    /// place. A new one each time faded in from grey again at every step, so the
    /// line flashed grey and green for as long as the download ran.
    pub(crate) fn update_sticky(slot: &mut Option<Self>, text: String) {
        match slot {
            Some(shown) if !shown.expired() => {
                shown.text = text;
                shown.expires = None;
                shown.warning = false;
            }
            _ => *slot = Some(Self::sticky(text)),
        }
    }
}

pub(crate) enum UpdateNotice {
    Available { version: String, asset_name: String, download_url: String, checksums_url: Option<String> },
    /// Drawn only once the user asks to watch (/update), so an unrequested update
    /// never takes over the screen.
    Progress { version: String, stage: flashagent_svc::updater::UpdateProgress },
    Ready { version: String },
    UpToDate { version: String },
    Failed { error: String },
}

impl App {
    /// Under the cursor, not in the transcript: it is for the person at the
    /// keyboard, not the conversation. A suggestion in the same spot is cleared.
    pub(crate) fn notice(&mut self, text: impl Into<String>) {
        self.custom_placeholder = Some(text.into());
        self.suggested_prompt = None;
        self.renderer.request_reprint();
    }
}

/// E.g. `Update b235 · ████████──────── 52% · 3.5/6.7 MB`.
pub(crate) fn update_progress_line(version: &str, stage: flashagent_svc::updater::UpdateProgress) -> String {
    use flashagent_svc::updater::UpdateProgress;
    pub(crate) const MB: f64 = 1024.0 * 1024.0;
    let detail = match stage {
        UpdateProgress::Downloading { received, total: Some(total) } if total > 0 => {
            let done = (received.min(total) as f64 / total as f64).clamp(0.0, 1.0);
            let filled = (done * 16.0).round() as usize;
            format!(
                "{}{} {:>3}% · {:.1}/{:.1} MB",
                "\u{2588}".repeat(filled),
                "\u{2500}".repeat(16 - filled),
                (done * 100.0).round() as u32,
                received as f64 / MB,
                total as f64 / MB
            )
        }
        // Some mirrors send no content-length; show the bytes, not a fake bar.
        UpdateProgress::Downloading { received, .. } => {
            format!("{:.1} MB downloaded", received as f64 / MB)
        }
        UpdateProgress::Verifying => "verifying the checksum\u{2026}".to_string(),
        UpdateProgress::Installing => "installing\u{2026}".to_string(),
    };
    format!("{UPDATE_LINE_PREFIX}{version} \u{b7} {detail}")
}

/// Marks the one line that update progress rewrites in place.
pub(crate) const UPDATE_LINE_PREFIX: &str = "Update ";

/// What /update does, given what is already going on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateKeyAction {
    /// A build from source never replaces itself.
    DevMode,
    /// Background or not: show it.
    ShowProgress,
    /// Wait for its answer instead of starting another.
    WatchCheck,
    /// Found earlier: install it.
    InstallPending,
    /// Check, and install what is found.
    CheckAndInstall,
}

pub(crate) fn update_key_action(dev_mode: bool, downloading: bool, busy: bool, pending: bool) -> UpdateKeyAction {
    if dev_mode {
        UpdateKeyAction::DevMode
    } else if downloading {
        UpdateKeyAction::ShowProgress
    } else if busy {
        UpdateKeyAction::WatchCheck
    } else if pending {
        UpdateKeyAction::InstallPending
    } else {
        UpdateKeyAction::CheckAndInstall
    }
}

#[cfg(test)]
mod update_key_tests {
    use super::*;

    #[test]
    fn ctrl_u_joins_an_update_already_under_way_instead_of_starting_a_second() {
        assert_eq!(update_key_action(false, true, true, false), UpdateKeyAction::ShowProgress);
        assert_eq!(update_key_action(false, false, true, true), UpdateKeyAction::WatchCheck);
        assert_eq!(update_key_action(false, false, false, true), UpdateKeyAction::InstallPending);
        assert_eq!(update_key_action(false, false, false, false), UpdateKeyAction::CheckAndInstall);
        assert_eq!(update_key_action(true, true, true, true), UpdateKeyAction::DevMode);
    }
}
