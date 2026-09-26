//! One model client for every protocol. It keeps what outlives a request (the
//! endpoint, the model, what the server was found to support and refuse) and
//! hands each request to the module for the protocol its endpoint speaks.

use std::sync::atomic::{AtomicBool, AtomicI8, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use parking_lot::RwLock;
use tokio::sync::mpsc;

use crate::protocol::{ApiProtocol, Endpoint};
use crate::thinking::{DiscoveredModel, ServerDiscovery, ServerKind, ThinkingProfile};
use crate::types::{ChatMessage, FinishReason, LlmError, LlmEvent, ThinkingEffort, ToolSpec, TurnOptions};

pub type EventStream = BoxStream<'static, Result<LlmEvent, LlmError>>;

pub struct Client {
    endpoint: RwLock<Endpoint>,
    /// Counts changes of endpoint, so what the previous server answered is
    /// not written over what the new one is found to be.
    generation: AtomicU64,
    model: RwLock<String>,
    pub(crate) http: reqwest::Client,
    profile: RwLock<Option<ThinkingProfile>>,
    discovery: RwLock<Option<ServerDiscovery>>,
    /// The model list that answered last, where a server has several.
    pub(crate) working_models_url: RwLock<Option<String>>,
    /// Retries after a connection failure only. HTTP errors and mid-stream drops
    /// are not retried: the request may already have had effects.
    max_retries: AtomicUsize,
    /// Sampling the user set by hand. Without it, APIs whose makers tune
    /// their own models (Anthropic, Gemini) get none of the presets, which are
    /// tuned for local models: Gemini 3 loops below temperature 1.0.
    user_sampling: AtomicBool,
    /// Auto effort shift in presets, learned from this model's past turns.
    effort_bias: AtomicI8,
    /// Model requests not yet finished, streams included. Background polling
    /// (the model list) waits for zero instead of competing with them.
    in_flight: Arc<AtomicUsize>,
    /// What this server rejected, kept for the model: without it every turn
    /// on a strict cloud API paid for a refused request first.
    pub(crate) learned: RwLock<crate::openai::Learned>,
}

/// Counts a request as running until dropped; a stream holds it to its last byte.
pub(crate) struct Busy(Arc<AtomicUsize>);

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Turns a response body into events. One per request; bytes arrive in order.
pub(crate) trait WireDecoder: Send + 'static {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Result<LlmEvent, LlmError>>;

    /// The body has ended: whatever is still buffered.
    fn finish(&mut self) -> Vec<Result<LlmEvent, LlmError>> {
        Vec::new()
    }
}

impl Client {
    pub fn new(endpoint: Endpoint, model: impl Into<String>) -> Self {
        Self {
            endpoint: RwLock::new(endpoint),
            generation: AtomicU64::new(0),
            model: RwLock::new(model.into()),
            // No total timeout: a slow local model can stream for many minutes. The idle
            // read timeout catches a server that stopped sending.
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .read_timeout(Duration::from_secs(300))
                .build()
                .unwrap_or_default(),
            profile: RwLock::new(None),
            discovery: RwLock::new(None),
            working_models_url: RwLock::new(None),
            max_retries: AtomicUsize::new(0),
            user_sampling: AtomicBool::new(false),
            effort_bias: AtomicI8::new(0),
            in_flight: Arc::new(AtomicUsize::new(0)),
            learned: RwLock::default(),
        }
    }

    pub fn endpoint(&self) -> Endpoint {
        self.endpoint.read().clone()
    }

    pub fn protocol(&self) -> ApiProtocol {
        self.endpoint.read().protocol
    }

    pub fn base_url(&self) -> String {
        self.endpoint.read().url.clone()
    }

    pub fn api_key(&self) -> Option<String> {
        self.endpoint.read().api_key.clone()
    }

    /// Another server, or the same one under another key or protocol. What
    /// was learned about the old one (its models, their thinking presets,
    /// the fields it refused) does not carry over. The model is kept: the
    /// caller picks one from the new server's list.
    pub fn set_endpoint(&self, endpoint: Endpoint) {
        let mut current = self.endpoint.write();
        if *current == endpoint {
            return;
        }
        *current = endpoint;
        // Under the endpoint lock, which `settle_discovery` holds while it
        // checks the generation and writes: the two never interleave.
        self.generation.fetch_add(1, Ordering::SeqCst);
        *self.discovery.write() = None;
        *self.working_models_url.write() = None;
        *self.profile.write() = None;
        *self.learned.write() = Default::default();
    }

    /// Read before anything about the server, and handed back with what it
    /// answered: an answer from before a change of endpoint is dropped.
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn requests_in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Relaxed)
    }

    pub(crate) fn busy(&self) -> Busy {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        Busy(self.in_flight.clone())
    }

    pub fn set_max_retries(&self, retries: usize) {
        self.max_retries.store(retries, Ordering::Relaxed);
    }

    /// True when the sampling was set by hand (the Custom preset): it then
    /// goes to every server, not only to local ones.
    pub fn set_user_sampling(&self, by_hand: bool) {
        self.user_sampling.store(by_hand, Ordering::Relaxed);
    }

    /// What goes out with a request: the caller's options, without sampling
    /// the provider should choose itself.
    pub(crate) fn sampling_for_protocol(&self, options: &TurnOptions) -> TurnOptions {
        let mut options = options.clone();
        let tuned_by_maker = matches!(self.protocol(), ApiProtocol::Anthropic | ApiProtocol::Gemini);
        if tuned_by_maker && !self.user_sampling.load(Ordering::Relaxed) {
            options.temperature = None;
            options.top_p = None;
            options.top_k = None;
            options.repeat_penalty = None;
            options.presence_penalty = None;
            options.min_p = None;
        }
        options
    }

    /// Only auto is affected; a preset the user picked by hand stays as is.
    pub fn set_effort_bias(&self, steps: i8) {
        self.effort_bias.store(steps.clamp(-1, 1), Ordering::Relaxed);
    }

    pub fn effort_bias(&self) -> i8 {
        self.effort_bias.load(Ordering::Relaxed)
    }

    pub fn model(&self) -> String {
        self.model.read().clone()
    }

    pub fn set_model(&self, model: impl Into<String>) {
        let model = model.into();
        {
            let mut current = self.model.write();
            if *current == model {
                return;
            }
            *current = model.clone();
        }
        // A thinking profile belongs to the model, so the old one is dropped;
        // otherwise a non-reasoning model gets asked for a reasoning effort.
        let derived = self
            .discovery
            .read()
            .as_ref()
            .and_then(|disc| disc.model(&model).map(|m| m.thinking.clone()));
        *self.profile.write() = derived;
        *self.learned.write() = Default::default();
    }

    pub fn discovery(&self) -> Option<ServerDiscovery> {
        self.discovery.read().clone()
    }

    /// Also checks `active_model`: some servers do not list the loaded model in
    /// `models`, and without the fallback no profile was adopted.
    pub fn adopt_discovery(&self, disc: &ServerDiscovery) {
        let model = self.model();
        let thinking = disc.model(&model).map(|m| m.thinking.clone());
        *self.discovery.write() = Some(disc.clone());
        if let Some(thinking) = thinking {
            *self.profile.write() = Some(thinking);
        }
    }

    pub(crate) fn server_kind(&self) -> ServerKind {
        self.discovery.read().as_ref().map(|d| d.kind).unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn with_profile(self, profile: ThinkingProfile) -> Self {
        *self.profile.write() = Some(profile);
        self
    }

    pub fn profile(&self) -> Option<ThinkingProfile> {
        self.profile.read().clone()
    }

    pub(crate) fn set_profile(&self, profile: ThinkingProfile) {
        *self.profile.write() = Some(profile);
    }

    /// The models a server listed, whatever its protocol: picks the active
    /// one, adopts its thinking profile and keeps the list, with `models_url`
    /// if that is where the list was. `None`, and nothing kept, when the list
    /// is empty or the endpoint changed since `generation` was read.
    pub(crate) fn settle_discovery(
        &self,
        generation: u64,
        models: Vec<DiscoveredModel>,
        kind: ServerKind,
        models_url: Option<String>,
    ) -> Option<ServerDiscovery> {
        if models.is_empty() {
            return None;
        }
        // Held to the end; nothing below may read the endpoint again, as a
        // second read behind a waiting `set_endpoint` would deadlock.
        let endpoint = self.endpoint.read();
        if self.generation() != generation {
            return None;
        }
        let current = self.model();
        // Ollama loads any installed model on demand, so there the model the
        // user chose stays chosen; preferring a loaded one would undo a pick at
        // the next look.
        let on_demand = kind == ServerKind::Ollama;
        // The exact name before a similar one: `gpt-4o` must not become
        // `gpt-4o-audio-preview` because the list happens to name that first.
        let exact = |m: &DiscoveredModel| !current.is_empty() && (m.id == current || (on_demand && m.id == format!("{current}:latest")));
        // A server that takes a key bills by the model: one the user did not
        // name is never chosen for them, however close its name. A typo stays
        // a typo, and the request says so. A local server serves what it has.
        let pick_for_user = current.is_empty() || kind.runs_local_models() || endpoint.api_key.is_none();
        let similar = |m: &DiscoveredModel| pick_for_user && !current.is_empty() && (m.id.contains(&current) || current.contains(&m.id));
        let active = models
            .iter()
            .find(|m| (m.is_loaded || on_demand) && exact(m))
            .or_else(|| models.iter().find(|m| m.is_loaded && similar(m)))
            .or_else(|| models.iter().find(|m| m.is_loaded))
            .or_else(|| models.iter().find(|m| exact(m)))
            .or_else(|| models.iter().find(|m| similar(m)))
            .or_else(|| models.first().filter(|_| pick_for_user))
            .cloned();

        if let Some(ref active) = active {
            self.set_model(&active.id);
            let mut lock = self.profile.write();
            // A listing that says nothing about reasoning keeps what an error taught.
            if active.thinking.supported || lock.is_none() {
                *lock = Some(active.thinking.clone());
            }
        }

        if models_url.is_some() {
            *self.working_models_url.write() = models_url;
        }
        let disc = ServerDiscovery { base_url: endpoint.url.clone(), models, active_model: active, kind };
        *self.discovery.write() = Some(disc.clone());
        Some(disc)
    }

    /// The effort this request asks for, as a preset name, or `None` for the
    /// model's own default.
    pub(crate) fn resolve_effort(&self, messages: &[ChatMessage], options: &TurnOptions) -> Option<String> {
        // Unknown abilities default to nothing: a guessed preset puts fields in the
        // request that the server warns about.
        let profile = self.profile().unwrap_or_default();
        if let Some(effort) = &options.custom_effort {
            return Some(effort.clone());
        }
        match options.thinking {
            ThinkingEffort::Off => Some(profile.resolve_effort(options.thinking).unwrap_or("off").to_string()),
            ThinkingEffort::Auto => profile.resolve_dynamic_biased(messages, self.effort_bias()).map(String::from),
            ThinkingEffort::Default => profile.default_preset.clone(),
            _ => profile.resolve_effort(options.thinking).map(String::from),
        }
    }

    /// POSTs `body`. Rate limits and an overloaded gateway are waited out when
    /// the server's wait is short; a connection that never opened is retried
    /// as configured. Anything else is returned as it came.
    pub(crate) async fn post(
        &self,
        url: &str,
        headers: &reqwest::header::HeaderMap,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, LlmError> {
        let retries = self.max_retries.load(Ordering::Relaxed);
        let mut attempt = 0usize;
        loop {
            let req = self.http.post(url).headers(headers.clone()).json(body);
            match req.send().await {
                // Nothing was generated: rate limits and an overloaded gateway
                // are worth waiting for, as long as the server's wait is short.
                Ok(resp) if attempt < retries.max(2) && matches!(resp.status().as_u16(), 429 | 502 | 503 | 504 | 529) => {
                    attempt += 1;
                    let wait = resp
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<f64>().ok())
                        // Negative, NaN or infinite would panic in `from_secs_f64`.
                        .and_then(|s| Duration::try_from_secs_f64(s).ok())
                        .unwrap_or(Duration::from_secs(2u64.pow(attempt as u32)));
                    if wait > Duration::from_secs(30) {
                        return Ok(resp);
                    }
                    tokio::time::sleep(wait).await;
                }
                Ok(resp) => return Ok(resp),
                // Connect errors only: after connecting, the server may already be generating.
                Err(e) if attempt < retries && e.is_connect() => {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// A GET that answers JSON within `timeout`, or `None`.
    pub(crate) async fn get_json(
        &self,
        url: &str,
        headers: &reqwest::header::HeaderMap,
        timeout: Duration,
    ) -> Option<serde_json::Value> {
        let resp = self.http.get(url).headers(headers.clone()).timeout(timeout).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json().await.ok()
    }

    /// Asks the server what it runs: the models, their context windows,
    /// whether they reason, see images and call tools.
    pub async fn discover_server(&self) -> Option<ServerDiscovery> {
        // First, so an endpoint changed while the server is asked is noticed.
        let generation = self.generation();
        match self.protocol() {
            ApiProtocol::OpenAi => crate::openai::discover(self, generation).await,
            ApiProtocol::Anthropic => crate::anthropic::discover(self, generation).await,
            ApiProtocol::Gemini => crate::gemini::discover(self, generation).await,
            ApiProtocol::Ollama => crate::ollama::discover(self, generation).await,
        }
    }

    /// Measured, not guessed: Qwen-VL charges by area, Gemma a flat rate per
    /// image. Returns `(per_pixel, fixed)`, or `None` where the protocol
    /// cannot tell.
    pub async fn measure_image_cost(&self, probe_png: &[u8], width: u32, height: u32) -> Option<(f32, f32)> {
        match self.protocol() {
            ApiProtocol::OpenAi => crate::openai::measure_image_cost(self, probe_png, width, height).await,
            _ => None,
        }
    }
}

/// Streams a successful response through `decoder`. The request counts as
/// running until the last byte, and a stream that ends without saying why
/// ends with [`FinishReason::Stop`].
pub(crate) fn pump(resp: reqwest::Response, busy: Busy, mut decoder: impl WireDecoder) -> EventStream {
    let (tx, rx) = mpsc::channel::<Result<LlmEvent, LlmError>>(256);
    tokio::spawn(async move {
        // The server keeps generating while this task reads.
        let _busy = busy;
        let mut done_sent = false;
        let mut body = resp.bytes_stream();
        while let Some(chunk) = body.next().await {
            let items = match chunk {
                Ok(bytes) => decoder.feed(&bytes),
                Err(e) => vec![Err(LlmError::Stream(e.to_string()))],
            };
            if !forward(&tx, items, &mut done_sent).await {
                return;
            }
        }
        if forward(&tx, decoder.finish(), &mut done_sent).await && !done_sent {
            let _ = tx.send(Ok(LlmEvent::Done(FinishReason::Stop))).await;
        }
    });
    Box::pin(futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|item| (item, rx)) }))
}

/// False once the reader is gone or an error has been passed on: an error ends the stream.
async fn forward(
    tx: &mpsc::Sender<Result<LlmEvent, LlmError>>,
    items: Vec<Result<LlmEvent, LlmError>>,
    done_sent: &mut bool,
) -> bool {
    for item in items {
        let failed = item.is_err();
        *done_sent |= matches!(item, Ok(LlmEvent::Done(_)));
        if tx.send(item).await.is_err() || failed {
            return false;
        }
    }
    true
}

#[async_trait]
impl crate::LlmBackend for Client {
    fn name(&self) -> &str {
        self.protocol().id()
    }

    async fn stream(&self, messages: &[ChatMessage], tools: &[ToolSpec]) -> Result<EventStream, LlmError> {
        self.stream_with_options(messages, tools, &TurnOptions::default()).await
    }

    async fn stream_with_options(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        options: &TurnOptions,
    ) -> Result<EventStream, LlmError> {
        let options = &self.sampling_for_protocol(options);
        match self.protocol() {
            ApiProtocol::OpenAi => crate::openai::stream(self, messages, tools, options).await,
            ApiProtocol::Anthropic => crate::anthropic::stream(self, messages, tools, options).await,
            ApiProtocol::Gemini => crate::gemini::stream(self, messages, tools, options).await,
            ApiProtocol::Ollama => crate::ollama::stream(self, messages, tools, options).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(model: &str) -> Client {
        Client::new(Endpoint::new(ApiProtocol::OpenAi, "http://localhost:1234/v1", None), model)
    }

    #[test]
    fn switching_models_does_not_keep_the_old_model_s_thinking_profile() {
        // Regression: the previous model's profile was kept after a model switch.
        let llm = client("thinker").with_profile(ThinkingProfile {
            presets: vec!["off".into(), "on".into()],
            protocol: crate::thinking::ThinkingProtocol::LmStudio,
            supported: true,
            default_preset: Some("on".into()),
        });
        assert!(llm.profile().is_some_and(|p| p.supported));
        llm.set_model("a-model-that-cannot-reason");
        assert!(llm.profile().is_none(), "an unknown model inherits nothing from the one before it");
        llm.set_model("a-model-that-cannot-reason");
        assert!(llm.profile().is_none(), "setting the same model again changes nothing");
    }

    #[test]
    fn the_learned_effort_correction_is_never_more_than_one_step() {
        // Clamped, so a bad streak cannot pin the model at "off".
        let llm = client("m");
        assert_eq!(llm.effort_bias(), 0, "nothing learned yet");
        llm.set_effort_bias(-7);
        assert_eq!(llm.effort_bias(), -1);
        llm.set_effort_bias(7);
        assert_eq!(llm.effort_bias(), 1);
        llm.set_effort_bias(0);
        assert_eq!(llm.effort_bias(), 0);
    }

    #[test]
    fn a_maker_tuned_api_chooses_its_own_sampling_unless_the_user_set_it_by_hand() {
        let options = TurnOptions { temperature: Some(0.6), top_p: Some(0.95), top_k: Some(20), min_p: Some(0.0), max_tokens: Some(100), ..Default::default() };
        let llm = client("m");
        assert_eq!(llm.sampling_for_protocol(&options), options, "local servers get the preset");
        for protocol in [ApiProtocol::Anthropic, ApiProtocol::Gemini] {
            llm.set_endpoint(Endpoint::new(protocol, "https://example.invalid", None));
            let sent = llm.sampling_for_protocol(&options);
            assert_eq!((sent.temperature, sent.top_p, sent.top_k, sent.min_p), (None, None, None, None), "{protocol}");
            assert_eq!(sent.max_tokens, Some(100), "the output cap is not sampling");
            llm.set_user_sampling(true);
            assert_eq!(llm.sampling_for_protocol(&options), options, "{protocol}: set by hand, sent as set");
            llm.set_user_sampling(false);
        }
    }

    #[test]
    fn a_model_a_keyed_server_does_not_list_is_never_swapped_for_another() {
        let listed = |id: &str| DiscoveredModel {
            id: id.into(),
            display_name: None,
            is_loaded: false,
            context_length: None,
            max_context_length: None,
            thinking: ThinkingProfile::unreported(),
            supports_tools: true,
            supports_vision: false,
        };
        let models = || vec![listed("typesafe/jev-router"), listed("stealth/space-bunny-alpha"), listed("stealth/space-bunny-alpha-pro")];
        // One letter off, on OpenRouter: the first listed model was taken, and billed.
        let cloud = Client::new(Endpoint::new(ApiProtocol::OpenAi, "https://openrouter.ai/api/v1", Some("k".into())), "stealth/space-bunny-alfa");
        let disc = cloud.settle_discovery(0, models(), ServerKind::Other, None).unwrap();
        assert_eq!(cloud.model(), "stealth/space-bunny-alfa");
        assert!(disc.active_model.is_none());
        // A part of a name is not completed to a model the user did not name either.
        let cloud = Client::new(Endpoint::new(ApiProtocol::OpenAi, "https://openrouter.ai/api/v1", Some("k".into())), "stealth/space-bunny");
        cloud.settle_discovery(0, models(), ServerKind::Other, None);
        assert_eq!(cloud.model(), "stealth/space-bunny");
        // The exact name is taken, and nothing saved means the server picks.
        let cloud = Client::new(Endpoint::new(ApiProtocol::OpenAi, "https://openrouter.ai/api/v1", Some("k".into())), "stealth/space-bunny-alpha");
        assert_eq!(cloud.settle_discovery(0, models(), ServerKind::Other, None).unwrap().active_model.unwrap().id, "stealth/space-bunny-alpha");
        let cloud = Client::new(Endpoint::new(ApiProtocol::OpenAi, "https://openrouter.ai/api/v1", Some("k".into())), "");
        cloud.settle_discovery(0, models(), ServerKind::Other, None);
        assert_eq!(cloud.model(), "typesafe/jev-router");
        // A local server without a key still serves what it has.
        let local = Client::new(Endpoint::new(ApiProtocol::OpenAi, "http://localhost:8000/v1", None), "gone-model");
        local.settle_discovery(0, models(), ServerKind::Other, None);
        assert_eq!(local.model(), "typesafe/jev-router");
    }

    #[test]
    fn another_server_starts_with_nothing_learned_about_the_last_one() {
        let llm = client("m").with_profile(ThinkingProfile {
            presets: vec!["low".into(), "high".into()],
            protocol: crate::thinking::ThinkingProtocol::ReasoningEffort,
            supported: true,
            default_preset: None,
        });
        llm.settle_discovery(
            0,
            vec![DiscoveredModel {
                id: "m".into(),
                display_name: None,
                is_loaded: true,
                context_length: Some(8192),
                max_context_length: None,
                thinking: ThinkingProfile::unreported(),
                supports_tools: true,
                supports_vision: false,
            }],
            ServerKind::LmStudio,
            None,
        );
        assert!(llm.discovery().is_some());

        llm.set_endpoint(Endpoint::new(ApiProtocol::OpenAi, "http://localhost:1234/v1/", None));
        assert!(llm.discovery().is_some(), "the same endpoint, written differently, is not a change");

        llm.set_endpoint(Endpoint::new(ApiProtocol::Anthropic, "https://api.anthropic.com", Some("k".into())));
        assert_eq!(llm.protocol(), ApiProtocol::Anthropic);
        assert_eq!(llm.base_url(), "https://api.anthropic.com");
        assert!(llm.discovery().is_none() && llm.profile().is_none());
        assert_eq!(llm.model(), "m", "the caller picks the next model");
        assert_eq!(crate::LlmBackend::name(&llm), "anthropic");
    }

    #[tokio::test]
    async fn what_the_server_left_behind_answers_is_not_kept() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // Holds every answer until the client has moved on.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let old = format!("http://{}", listener.local_addr().unwrap());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let (asked_tx, mut asked) = mpsc::unbounded_channel();
        let held = release.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let (held, asked_tx) = (held.clone(), asked_tx.clone());
                tokio::spawn(async move {
                    let _ = sock.read(&mut [0u8; 4096]).await;
                    let _ = asked_tx.send(());
                    let _permit = held.acquire().await;
                    let body = r#"{"data":[{"id":"old-model"}]}"#;
                    let head = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                    let _ = sock.write_all(format!("{head}{body}").as_bytes()).await;
                });
            }
        });

        let llm = Client::new(Endpoint::new(ApiProtocol::OpenAi, format!("{old}/v1"), None), "new-model");
        let switch = async {
            asked.recv().await;
            llm.set_endpoint(Endpoint::new(ApiProtocol::OpenAi, "http://127.0.0.1:9/v1", None));
            release.add_permits(16);
        };
        let (found, ()) = tokio::join!(llm.discover_server(), switch);
        assert!(found.is_none(), "the old server's list came back as the new one's");
        assert!(llm.discovery().is_none());
        assert_eq!(llm.model(), "new-model", "the old server's model replaced the chosen one");
        assert!(llm.working_models_url.read().is_none());
    }
}
