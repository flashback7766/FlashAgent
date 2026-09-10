# 2026-09-07 — B0 Phase 2: Interactive Prototype (Assembly)

### Status: PASS (code) / PARTIAL (gate overall — awaiting owner verdict)
### Decision: B0 (renderer prototype, go/no-go gate)

Files touched:
- crates/ui/Cargo.toml (+wgpu, winit, pollster, bytemuck, swash)
- crates/ui/src/bin/b0.rs (new, ~720 lines)
- Cargo.toml (workspace: +pollster, +swash)

Verification:
- `cargo clippy -p flashagent-ui --bins -- -D warnings` → Finished, 0 errors/warnings (exit 0)
- `cargo test -p flashagent-ui` → 5 passed; 0 failed (exit 0)
- `cargo build -p flashagent-ui --bins` → Finished (exit 0)
- Launch impossible in sandbox (headless, no Vulkan/GL): visual check on owner machine.

Architecture inside b0.rs:
- winit 0.30 window + wgpu 27 pipeline (triangle list, alpha-blend, RGBA glyph atlas 1024x2048)
- cosmic-text shaping + swash rasterization, Mono/Proportional, live composer
- IME: Preedit (accent color + underline) / Commit
- Spring physics (k=170, c=14) button morph on click
- Looping background animations (cursor blink, title pulse) in parallel
- Cyrillic hint string, Esc/Enter/Backspace, DPR scaling, Fifo vsync
- Known limitation: wgpu 27 lacks documented into_static — used transmute lifetime surface with SAFETY justification (Window in App, field declared before Gpu, dropped later).

Open questions:
- Gate B0 verdict: FPS, text rendering feel, morph quality — on owner machine.

Handoff:
- Owner: `cargo run -p flashagent-ui --bin b0 --release` (Windows/Linux desktop).
- On GO: mark B0 [x] in ROADMAP.md, start B1 (M3 Expressive kit) and A0 in parallel.

## Addendum (after owner initial run)
- Owner ran prototype: window, Cyrillic, composer, IME, spring morph — functioning.
- Discovered: colors faded (sRGB double-conversion). Fix: select non-sRGB surface format (values treated literally). Applied, clippy strict green.
- Awaiting: second run by owner + go/no-go verdict on gate B0.

## Addendum 2
- Colors after format fix: confirmed via screenshot (black background, M3 purple button).
- Bug found: space character was not inserted (winit sends Named(Space), not Character). Fixed.
