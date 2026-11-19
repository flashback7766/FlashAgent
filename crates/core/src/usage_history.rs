//! Usage kept across runs: `~/.flashagent/usage.json`.
//!
//! The session's ledger is reset when another session opens, and a saved
//! session has no room for a whole bill's worth of detail. This file is the
//! lifetime view: the same [`UsageTotals`] per day, per provider and per model,
//! added to on every usage report and written once a turn ends. Nothing here is
//! estimated: an API that prices nothing adds no cost, and the request count
//! still says how much work happened.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cost::UsageTotals;

/// Days kept in the file. A year of rows is more than the menu reads, and a
/// file that only grows is a file that eventually costs more to parse than it
/// says.
const KEPT_DAYS: usize = 400;

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct UsageHistory {
    /// Unix seconds of the first record in this file.
    pub since: u64,
    pub totals: UsageTotals,
    /// `YYYY-MM-DD` (UTC) → that day's totals.
    pub days: BTreeMap<String, UsageTotals>,
    pub providers: BTreeMap<String, UsageTotals>,
    pub models: BTreeMap<String, UsageTotals>,
}

impl UsageHistory {
    /// Adds one request. `at` is a Unix timestamp in seconds.
    pub fn record(&mut self, provider: &str, model: &str, usage: &flashagent_llm::Usage, at: u64) {
        if self.since == 0 {
            self.since = at;
        }
        self.totals.add_usage(usage);
        self.days.entry(day_key(at)).or_default().add_usage(usage);
        self.providers.entry(provider.to_string()).or_default().add_usage(usage);
        self.models.entry(model.to_string()).or_default().add_usage(usage);
        if self.days.len() > KEPT_DAYS {
            let keep_from = self.days.keys().rev().nth(KEPT_DAYS - 1).cloned();
            if let Some(keep_from) = keep_from {
                self.days = self.days.split_off(&keep_from);
            }
        }
    }

    /// Newest first, for the menu's daily rows.
    pub fn recent_days(&self, count: usize) -> Vec<(&str, &UsageTotals)> {
        self.days.iter().rev().take(count).map(|(day, totals)| (day.as_str(), totals)).collect()
    }

    /// Most spent first, then by tokens.
    pub fn by_provider(&self) -> Vec<(&str, &UsageTotals)> {
        let mut rows: Vec<(&str, &UsageTotals)> = self.providers.iter().map(|(name, totals)| (name.as_str(), totals)).collect();
        rows.sort_by(|a, b| {
            b.1.cost
                .partial_cmp(&a.1.cost)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.total_tokens().cmp(&a.1.total_tokens()))
                .then_with(|| a.0.cmp(b.0))
        });
        rows
    }

    /// Most spent first, then by tokens.
    pub fn by_model(&self) -> Vec<(&str, &UsageTotals)> {
        let mut rows: Vec<(&str, &UsageTotals)> = self.models.iter().map(|(name, totals)| (name.as_str(), totals)).collect();
        rows.sort_by(|a, b| {
            b.1.cost
                .partial_cmp(&a.1.cost)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.total_tokens().cmp(&a.1.total_tokens()))
                .then_with(|| a.0.cmp(b.0))
        });
        rows
    }
}

/// `YYYY-MM-DD` in UTC. The file is read on another day and possibly in
/// another timezone; a date that shifts with the machine is not a date.
pub fn day_key(at: u64) -> String {
    let days = (at / 86_400) as i64;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → calendar date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

pub fn usage_history_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|h| PathBuf::from(h).join(".flashagent").join("usage.json"))
}

/// A missing or broken file means nothing has been recorded yet: the menu is
/// not worth refusing to open over a counter.
pub fn load(path: &Path) -> UsageHistory {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Written beside the final name and renamed, so a crash mid-write leaves the
/// previous file whole. Mode 0600: it names the owner's providers and models.
pub fn save(path: &Path, history: &UsageHistory) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let tmp = parent.join(format!("usage.json.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(history).map_err(std::io::Error::other)?;
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(prompt: i64, completion: i64, cost: Option<f64>) -> flashagent_llm::Usage {
        flashagent_llm::Usage { prompt: Some(prompt), completion: Some(completion), cost, ..Default::default() }
    }

    #[test]
    fn days_are_named_the_way_a_calendar_names_them() {
        assert_eq!(day_key(0), "1970-01-01");
        assert_eq!(day_key(1_700_000_000), "2023-11-14");
        assert_eq!(day_key(1_700_000_000 + 86_400), "2023-11-15");
    }

    #[test]
    fn one_request_reaches_the_totals_the_day_the_provider_and_the_model() {
        let mut history = UsageHistory::default();
        let at = 1_700_000_000;
        history.record("OpenRouter", "alpha", &usage(1_000, 200, Some(0.002)), at);
        history.record("OpenRouter", "beta", &usage(500, 100, Some(0.001)), at + 60);
        history.record("Local", "gamma", &usage(300, 50, None), at + 86_400);
        assert_eq!(history.totals.requests, 3);
        assert_eq!(history.totals.total_tokens(), 2_150);
        assert_eq!(history.days.len(), 2);
        let days = history.recent_days(10);
        assert_eq!(days[0].0, "2023-11-15");
        assert_eq!(days[1].1.total_tokens(), 1_800);
        assert_eq!(history.by_provider()[0].0, "OpenRouter");
        assert_eq!(history.by_model().len(), 3);
        assert!((history.totals.cost - 0.003).abs() < 1e-9);
    }

    #[test]
    fn a_file_round_trips_and_a_broken_one_is_empty_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let mut history = UsageHistory::default();
        history.record("P", "m", &usage(10, 2, Some(0.5)), 1_700_000_000);
        save(&path, &history).unwrap();
        assert_eq!(load(&path), history);
        assert_eq!(load(&dir.path().join("missing.json")), UsageHistory::default());
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load(&path), UsageHistory::default(), "a corrupt counter must not block the menu");
    }

    #[test]
    fn an_old_file_without_the_new_fields_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        std::fs::write(&path, r#"{"since":1700000000,"totals":{"requests":4,"cost":0.25}}"#).unwrap();
        let history = load(&path);
        assert_eq!(history.totals.requests, 4);
        assert!((history.totals.cost - 0.25).abs() < 1e-9);
        assert!(history.days.is_empty());
    }
}
