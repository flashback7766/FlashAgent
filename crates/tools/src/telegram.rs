//! The bridge to the bot: Telegram actions are validated here and executed
//! there.
//!
//! The serve never holds the Telegram token — the bot does, and it re-checks
//! every call before it reaches the API (the double guard). The tool's job on
//! this side is to refuse nonsense before the network round trip: a ban with
//! no target, a mute of forty years, a cross-room action the session has no
//! business doing. What the tool answers is a short verdict line, so the
//! model can react to a rejection instead of guessing.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::{parse_args, ToolError};

/// The bridge connection a tgbot serve is built with. `cross_room` marks the
/// dev session: only it may act outside its own room (`target: users|channel`).
pub struct TgBridgeConfig {
    pub base_url: String,
    pub token: String,
    pub cross_room: bool,
    /// The room this serve IS (`dev` / `users`): the bot defaults every
    /// action and journal read to it. Without it a users' serve read the
    /// dev room's journal and aimed its verdicts at the wrong chat.
    pub room: Option<String>,
}

/// How many mutating calls one turn may make, and how fast they may follow
/// each other. The budget lives in a task-local: every turn gets a fresh one,
/// parallel turns (report threads, the moderator) never share their counters.
pub struct TurnBudget {
    mutations: u32,
    last_at: Option<Instant>,
}

/// The per-turn cap over mutating Telegram and GitHub calls together.
pub const TURN_MUTATION_CAP: u32 = 8;
/// The pause two mutating calls must keep between themselves.
pub const MUTATION_THROTTLE: Duration = Duration::from_secs(2);

impl TurnBudget {
    pub fn new() -> Self {
        Self { mutations: 0, last_at: None }
    }

    /// Charges one mutating call, or names the limit the model just hit. The
    /// verdict is a tool error on purpose: the model must see the refusal as
    /// the call's answer, not as something to retry blind.
    fn charge(&mut self) -> Result<(), ToolError> {
        if self.mutations >= TURN_MUTATION_CAP {
            return Err(ToolError::Other(format!(
                "err: лимит хода исчерпан — не более {TURN_MUTATION_CAP} действий за ход; закончи ход"
            )));
        }
        if let Some(at) = self.last_at {
            let spent = at.elapsed();
            if spent < MUTATION_THROTTLE {
                let wait = (MUTATION_THROTTLE - spent).as_millis().max(1);
                return Err(ToolError::Other(format!(
                    "err: слишком часто — подожди {wait} мс до следующего действия"
                )));
            }
        }
        self.mutations += 1;
        self.last_at = Some(Instant::now());
        Ok(())
    }

    /// Tests only: a warm counter, so the cap test skips the clock. The
    /// throttle keeps its own test.
    #[cfg(test)]
    fn force_count(&mut self, n: u32) {
        self.mutations = n;
    }
}

tokio::task_local! {
    /// The turn's budget, scoped by the serve around every conversation turn.
    static TURN_BUDGET: RefCell<TurnBudget>;
}

/// Runs one turn's future with its own fresh budget.
pub async fn scope_turn<F: std::future::Future>(f: F) -> F::Output {
    TURN_BUDGET.scope(RefCell::new(TurnBudget::new()), f).await
}

/// True when the current task carries a turn budget (a serve turn does).
pub fn has_turn_scope() -> bool {
    TURN_BUDGET.try_with(|_| ()).is_ok()
}

/// Charges one mutation against the current turn's budget. The GitHub tool
/// shares this cap with the Telegram tool (A8: ТГ и GitHub вместе).
pub fn charge_shared_mutation() -> Result<(), ToolError> {
    TURN_BUDGET
        .try_with(|b| b.borrow_mut().charge())
        .unwrap_or_else(|_| {
            Err(ToolError::Other(
                "err: ход не размечен — тул живёт только внутри хода serve".into(),
            ))
        })
}

/// The `telegram` tool's arguments. Action-specific ones are checked by hand:
/// the schema marks them optional, and a Russian error beats a serde one.
#[derive(Deserialize)]
pub struct TelegramArgs {
    pub action: String,
    pub target: Option<String>,
    pub msg_id: Option<i64>,
    pub text: Option<String>,
    pub user: Option<String>,
    pub minutes: Option<i64>,
    pub reason: Option<String>,
    pub emoji: Option<String>,
}

/// The `tg_journal` tool's arguments: everything is optional, the empty call
/// reads the room's recent lines.
#[derive(Deserialize)]
pub struct TgJournalArgs {
    pub minutes: Option<i64>,
    pub last: Option<i64>,
    pub user: Option<String>,
    pub query: Option<String>,
    pub threads: Option<bool>,
}

/// The actions that change the room. `reply` is speech — the room's whole
/// point — and reads are reads; everything else burns budget.
fn is_mutation(action: &str) -> bool {
    matches!(
        action,
        "delete" | "ban" | "unban" | "mute" | "unmute" | "pin" | "unpin" | "react"
            | "publish" | "forget_user"
    )
}

/// How long the tool waits for the bot (H14). A slower bridge is a stalled
/// turn; three seconds is a verdict, not a wait.
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(3);

pub struct TgBridge {
    http: reqwest::Client,
    cfg: TgBridgeConfig,
}

impl TgBridge {
    pub fn new(cfg: TgBridgeConfig) -> Result<Self, ToolError> {
        let http = reqwest::Client::builder()
            .timeout(BRIDGE_TIMEOUT)
            .connect_timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| ToolError::Other(format!("bridge client: {e}")))?;
        Ok(Self { http, cfg })
    }

    /// One POST to the bot. The bot's verdict comes back as `{"ok":..}`; a
    /// rejection keeps its Russian text so the model sees why it was refused.
    async fn post(&self, body: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let url = format!("{}/bridge", self.cfg.base_url);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.cfg.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                // The bridge is down or timed out: say so honestly (A5), no
                // queues and no retries — a queued action is a duplicate later.
                ToolError::Other(format!("err: бот недоступен, повтори позже ({e})"))
            })?;
        let status = resp.status();
        let value: serde_json::Value = resp.json().await.map_err(|e| {
            ToolError::Other(format!("err: мост ответил не JSON (HTTP {status}) ({e})"))
        })?;
        // The bot's guards answer 200 with ok:false — their text is the answer.
        // A 4xx/5xx without a body is a broken contract; still a verdict line.
        if !value.get("ok").and_then(serde_json::Value::as_bool).unwrap_or(false) {
            let err = value
                .get("err")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("err: мост отклонил действие без объяснения");
            return Err(ToolError::Other(err.to_string()));
        }
        Ok(value)
    }

    /// The `telegram` tool: guard, charge, send, answer with one line (A10).
    pub async fn telegram(&self, args_json: &str) -> Result<String, ToolError> {
        let a: TelegramArgs = parse_args(args_json, "telegram")?;
        let action = a.action.trim().to_ascii_lowercase();

        // Cross-room targeting is a dev-session power (A13/A14): the names are
        // from the bot's config, never raw chat ids.
        if let Some(target) = a.target.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            if !self.cfg.cross_room {
                return Err(ToolError::Other(
                    "err: действия в чужих комнатах не разрешены этой сессии".into(),
                ));
            }
            if target != "users" && target != "channel" {
                return Err(ToolError::Other(format!(
                    "err: неизвестная комната '{target}' — бывают users и channel"
                )));
            }
        }
        // publish always goes to the channel, so it needs the same power.
        if action == "publish" && !self.cfg.cross_room {
            return Err(ToolError::Other(
                "err: публикация в канал не разрешена этой сессии".into(),
            ));
        }

        let need = |what: &str, v: &Option<String>| -> Result<(), ToolError> {
            if v.as_deref().map(str::trim).unwrap_or("").is_empty() {
                Err(ToolError::Other(format!("err: {action} требует {what}")))
            } else {
                Ok(())
            }
        };
        let need_msg = |v: Option<i64>| -> Result<i64, ToolError> {
            v.ok_or_else(|| ToolError::Other(format!("err: {action} требует msg_id")))
        };
        match action.as_str() {
            "reply" => {
                need_msg(a.msg_id)?;
                need("text", &a.text)?;
                if a.text.as_deref().map(str::trim).unwrap_or("").is_empty() {
                    return Err(ToolError::Other("err: reply с пустым текстом".into()));
                }
            }
            "delete" | "pin" | "unpin" | "react" => {
                need_msg(a.msg_id)?;
                // react without an emoji is fine: the bot applies the room's
                // default reaction (heart on fire); the model overrides it
                // when the case calls for another.
            }
            "ban" | "unban" | "unmute" | "forget_user" => {
                need("user", &a.user)?;
            }
            // Санити-потолок: модель решает длительность (F25), но не
            // на годы вперёд. Бот ещё раз проверит против своего конфига.
            "mute" => {
                need("user", &a.user)?;
                if let Some(minutes) = a.minutes {
                    if !(1..=43_200).contains(&minutes) {
                        return Err(ToolError::Other(
                            "err: минуты мута — целое от 1 до 43200 (30 дней)".into(),
                        ));
                    }
                }
            }
            "publish" => need("text", &a.text)?,
            "room_info" | "resolve" => {
                if action == "resolve" {
                    need("user", &a.user)?;
                }
            }
            other => {
                return Err(ToolError::Other(format!(
                    "err: неизвестное действие '{other}'"
                )))
            }
        }

        if is_mutation(&action) {
            charge_shared_mutation()?;
        }

        let mut body = serde_json::json!({ "action": action });
        if let Some(room) = self.cfg.room.as_deref().filter(|r| !r.is_empty()) {
            body["room"] = serde_json::json!(room);
        }
        for (key, value) in [
            ("target", a.target),
            ("text", a.text),
            ("user", a.user),
            ("reason", a.reason),
            ("emoji", a.emoji),
        ] {
            if let Some(v) = value {
                let v = v.trim().to_string();
                if !v.is_empty() {
                    body[key] = serde_json::json!(v);
                }
            }
        }
        if let Some(v) = a.msg_id {
            body["msg_id"] = serde_json::json!(v);
        }
        if let Some(v) = a.minutes {
            body["minutes"] = serde_json::json!(v);
        }
        let answer = self.post(body).await?;
        Ok(answer
            .get("result")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("ok")
            .to_string())
    }

    /// The `tg_journal` tool: a read, no budget, the bot does the filtering.
    pub async fn journal(&self, args_json: &str) -> Result<String, ToolError> {
        let a: TgJournalArgs = parse_args(args_json, "tg_journal")?;
        let mut body = serde_json::json!({ "action": "journal" });
        if let Some(room) = self.cfg.room.as_deref().filter(|r| !r.is_empty()) {
            body["room"] = serde_json::json!(room);
        }
        if let Some(v) = a.minutes {
            body["minutes"] = serde_json::json!(v);
        }
        if let Some(v) = a.last {
            body["last"] = serde_json::json!(v);
        }
        for (key, value) in [("user", a.user), ("query", a.query)] {
            if let Some(v) = value {
                let v = v.trim().to_string();
                if !v.is_empty() {
                    body[key] = serde_json::json!(v);
                }
            }
        }
        if a.threads.unwrap_or(false) {
            body["threads"] = serde_json::json!(true);
        }
        let answer = self.post(body).await?;
        Ok(answer
            .get("result")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge() -> TgBridge {
        TgBridge::new(TgBridgeConfig {
            base_url: "http://127.0.0.1:1".into(),
            token: "test".into(),
            cross_room: false,
            room: Some("users".into()),
        })
        .unwrap()
    }

    async fn call(args: serde_json::Value) -> Result<String, ToolError> {
        scope_turn(bridge().telegram(&args.to_string())).await
    }

    #[tokio::test]
    async fn a_reply_without_text_is_refused_before_the_network() {
        let err = call(serde_json::json!({"action": "reply", "msg_id": 5}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("reply требует text"), "{err}");
    }

    #[tokio::test]
    async fn a_cross_room_action_needs_the_power() {
        let err = call(serde_json::json!({"action": "ban", "user": "123", "target": "users"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("чужих комнатах"), "{err}");
    }

    #[tokio::test]
    async fn a_forty_year_mute_is_a_typo_not_a_verdict() {
        let err = call(
            serde_json::json!({"action": "mute", "user": "123", "minutes": 9_000_000}),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("43200"), "{err}");
    }

    #[tokio::test]
    async fn an_unknown_action_names_itself() {
        let err = call(serde_json::json!({"action": "explode"})).await.unwrap_err();
        assert!(err.to_string().contains("explode"), "{err}");
    }

    #[tokio::test]
    async fn the_ninth_mutation_is_the_cap_talking() {
        // Восемь зарядов уходят (дальше сеть бы отказала), девятый обязан
        // застрять на бюджете раньше сети. Все девять — в одном ходе, иначе
        // у каждого свой свежий бюджет.
        let outcomes = scope_turn(async {
            // Счётчик прогрет до последнего разрешённого действия: троттлинг
            // здесь не мешает (первый заряд идёт без паузы), потолок говорит
            // на втором.
            TURN_BUDGET.with(|b| b.borrow_mut().force_count(TURN_MUTATION_CAP - 1));
            let mut outcomes = Vec::new();
            for _ in 0..2 {
                outcomes.push(
                    TURN_BUDGET
                        .with(|b| b.borrow_mut().charge())
                        .and_then(|()| Err(ToolError::Other("network would fail here".into())))
                        .map(|_: ()| ())
                        .unwrap_err()
                        .to_string(),
                );
            }
            outcomes
        })
        .await;
        assert!(outcomes[0].contains("network would fail"), "{}", outcomes[0]);
        assert!(outcomes[1].contains("лимит хода"), "{}", outcomes[1]);
    }

    #[tokio::test]
    async fn the_throttle_keeps_two_mutations_apart() {
        let err = scope_turn(async {
            TURN_BUDGET.with(|b| {
                let mut b = b.borrow_mut();
                b.charge().unwrap();
                b.charge().unwrap_err()
            })
        })
        .await;
        assert!(err.to_string().contains("подожди"), "{err}");
    }
}
