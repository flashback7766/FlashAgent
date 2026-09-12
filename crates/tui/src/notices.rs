#[derive(Debug, Clone)]
/// A release-channel change waiting to be confirmed.
///
/// Switching channel is not a preference like a colour: it replaces the
/// binary with a different line of builds, which can take features away and
/// leave settings behind that the older build does not understand. It is
/// asked about, once, in those words.
pub(crate) struct ChannelSwitch {
    pub(crate) from: flashagent_core::config::UpdateChannel,
    pub(crate) to: flashagent_core::config::UpdateChannel,
    /// The version that channel would put you on, once the check answers.
    pub(crate) target: ChannelTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ChannelTarget {
    /// The release feed has not answered yet.
    Checking,
    /// The newest build on that channel.
    Version(String),
    /// The channel exists but has nothing published on it.
    Empty,
    /// The feed could not be reached; the switch is still the user's to make.
    Unknown,
}

/// What the user is about to do, in a sentence they can decide on.
pub(crate) fn channel_switch_warning(
    to: flashagent_core::config::UpdateChannel,
    current: &str,
    target: &ChannelTarget,
) -> String {
    use flashagent_core::config::UpdateChannel;
    let channel = to.label().to_lowercase();
    let destination = match target {
        ChannelTarget::Version(v) => v.clone(),
        ChannelTarget::Empty => {
            return format!(
                "Nothing is published on the {channel} channel yet, so you would stay on \
                 {current} until something is. Switch anyway?"
            )
        }
        // Still checking, or the feed could not be reached: name the channel
        // rather than invent a version.
        ChannelTarget::Checking | ChannelTarget::Unknown => format!("the newest {channel} release"),
    };
    match to {
        UpdateChannel::Stable => format!(
            "You will be moved from {current} down to {destination}. Features added since may \
             disappear or behave differently, and settings they introduced can be reset. Continue?"
        ),
        UpdateChannel::Beta => format!(
            "You will be moved from {current} to {destination}. Beta builds land often and can \
             regress; that is the point of them. Continue?"
        ),
    }
}

/// What the turn is doing right now, as far as the loop has told us.
///
/// Every state here is something the loop actually reported — the model has
/// not answered yet, it is reasoning, it is writing, a named tool is running.
/// Nothing is inferred from how long it has taken: "almost done" is not a
/// thing the program knows, and guessing it is how a progress bar starts
/// lying.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TurnPhase {
    /// Request sent, nothing back yet.
    Waiting,
    /// Reasoning tokens are arriving.
    Thinking,
    /// Visible answer tokens are arriving.
    Writing,
    /// A tool is running; the string is what it is doing.
    Tool(String),
    /// A tool finished and the model has the result but has not spoken yet.
    AfterTool,
    /// The user asked to stop and the turn is winding down.
    Stopping,
}

impl TurnPhase {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Waiting => "Waiting for the model".to_string(),
            Self::Thinking => "Thinking".to_string(),
            Self::Writing => "Writing the answer".to_string(),
            Self::Tool(what) => flashagent_tui::truncate_middle(what, 60),
            Self::AfterTool => "Reading the result".to_string(),
            Self::Stopping => "Stopping".to_string(),
        }
    }
}


/// A message that appeared without the user doing anything. It lives on the
/// line under the input, and most of them fade: a notice that is no longer
/// actionable should not take that line for the rest of the session.
pub(crate) struct BackgroundNotice {
    pub(crate) text: String,
    pub(crate) expires: Option<std::time::Instant>,
}

/// How brightly a background notice is drawn. A notice that is about to
/// expire dims over its last second, so it leaves the line instead of
/// blinking out of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct NoticeStyle {
    pub(crate) fade: f32,
}

impl NoticeStyle {
    pub(crate) const FULL: Self = Self { fade: 1.0 };

    pub(crate) fn paint(self, text: &str) -> String {
        let lerp = |from: f32, to: f32| (to + (from - to) * self.fade).round() as u8;
        // From the healthy green towards the muted grey the hints use.
        let (r, g, b) = (lerp(120.0, 100.0), lerp(220.0, 95.0), lerp(140.0, 90.0));
        let bold = if self.fade > 0.6 { "1;" } else { "" };
        format!("\x1b[{bold}38;2;{r};{g};{b}m{text}\x1b[0m")
    }
}

impl BackgroundNotice {
    /// Full brightness until the last second of its life, then a ramp down.
    pub(crate) fn style(&self) -> NoticeStyle {
        match self.expires {
            None => NoticeStyle::FULL,
            Some(at) => {
                let left = at.saturating_duration_since(std::time::Instant::now()).as_secs_f32();
                NoticeStyle { fade: left.clamp(0.0, 1.0) }
            }
        }
    }

    /// Stays until something replaces it — use for anything the user still
    /// has to act on ("restart to run the new build").
    pub(crate) fn sticky(text: impl Into<String>) -> Self {
        Self { text: text.into(), expires: None }
    }

    /// Disappears after `secs`.
    pub(crate) fn fading(text: impl Into<String>, secs: u64) -> Self {
        Self {
            text: text.into(),
            expires: Some(std::time::Instant::now() + std::time::Duration::from_secs(secs)),
        }
    }

    pub(crate) fn expired(&self) -> bool {
        self.expires.is_some_and(|t| std::time::Instant::now() >= t)
    }
}

pub(crate) enum UpdateNotice {
    Available { version: String, asset_name: String, download_url: String, checksums_url: Option<String> },
    /// Only the manual update (Ctrl+U) sends these; a background update stays
    /// silent so it never takes over a screen the user is reading.
    Progress { version: String, stage: flashagent_svc::updater::UpdateProgress },
    Ready { version: String },
    UpToDate { version: String },
    Failed { error: String },
}

/// One line of manual-update progress, e.g.
/// `Update b235 · [████████░░░░░░░░] 52% · 3.5/6.7 MB`.
pub(crate) fn update_progress_line(version: &str, stage: flashagent_svc::updater::UpdateProgress) -> String {
    use flashagent_svc::updater::UpdateProgress;
    pub(crate) const MB: f64 = 1024.0 * 1024.0;
    let detail = match stage {
        UpdateProgress::Downloading { received, total: Some(total) } if total > 0 => {
            let done = (received.min(total) as f64 / total as f64).clamp(0.0, 1.0);
            let filled = (done * 16.0).round() as usize;
            format!(
                "[{}{}] {:>3}% · {:.1}/{:.1} MB",
                "\u{2588}".repeat(filled),
                "\u{2591}".repeat(16 - filled),
                (done * 100.0).round() as u32,
                received as f64 / MB,
                total as f64 / MB
            )
        }
        // Some mirrors send no content-length; show the bytes, not a fake bar.
        UpdateProgress::Downloading { received, .. } => {
            format!("{:.1} MB downloaded", received as f64 / MB)
        }
        UpdateProgress::Verifying => "verifying checksum...".to_string(),
        UpdateProgress::Installing => "installing...".to_string(),
    };
    format!("{UPDATE_LINE_PREFIX}{version} \u{b7} {detail}")
}

/// Marks the single line that manual-update progress rewrites in place.
pub(crate) const UPDATE_LINE_PREFIX: &str = "Update ";
