# Audit: Turn Stats Display (Prompt Tokens, TG Speed, MTP Acceptance Rate)

### Status: PASS
Decision: UX Polish & Observability — display turn metrics (prompt tokens, generation speed `tg`, and speculative decoding acceptance rate `mtp`) right-aligned on Line 2 directly above the compact context gauge on Line 3.

Files touched:
- `crates/llm/src/types.rs`: added `MtpStats` struct and `pub mtp: Option<MtpStats>` to `Usage`.
- `crates/llm/src/lib.rs`: re-exported `MtpStats` in `types`.
- `crates/llm/src/parse.rs`: added MTP stats extraction (`stats`, `speculative_stats`, `completion_tokens_details`) and unit tests.
- `crates/core/src/loop_.rs`: updated `Usage` initialization in unit tests.
- `crates/tools/src/shell.rs`: fixed process wait race condition by awaiting stdout/stderr pump tasks.
- `crates/tui/src/main.rs`: extended `TokenTracker` to track prompt tokens, active streaming duration, overall generation throughput (`tg`), and `MtpStats`; updated `FrameState` and `Renderer::frame` Line 2 rendering with right-aligned layout vertically matching Line 3's context gauge; added unit tests.

Verification:
- `cargo test --workspace` (exit code: 0)
- `cargo clippy --workspace -- -D warnings` (exit code: 0)
- `cargo build --release --bin flashagent-tui` (exit code: 0)
- Live verification in tmux with `gemma-4-e2b-it-qat@q4_k_xl` on LM Studio:
  - Line 2 displayed `4.2K prompt · 82.5 tg · mtp: 100%` and `4.3K prompt · 52.6 tg · mtp: 62%`.
  - Line 3 displayed `❪▉─────────❫ 9% · 5.6K/64K` with matching right edge alignment.
  - Active streaming update displayed `4.9K prompt · 68.4 tg` in real-time.

Open questions:
- None.

Handoff:
- The stats display on Line 2 above the context gauge is fully active, tested live in tmux, and verified with all workspace checks.
