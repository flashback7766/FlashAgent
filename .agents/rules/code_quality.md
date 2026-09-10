# Code Quality — Rust

- `#![deny(warnings)]` ethos: strict clippy, no `allow` without an explanatory comment.
- No `unwrap()` / `expect()` outside of tests and pre-logger initialization. Errors use `thiserror` for libraries and `anyhow` exclusively in binaries.
- Crate boundaries are inviolable: `core` knows nothing of HTTP, SQL, or UI. Any breach is an architectural flaw.
- All public APIs documented (`///`, rustdoc passes with zero warnings).
- Strongly typed channels and events: no generic "JSON-string-for-everything".
- Tools and prompts are data, not code: toolsets adapt dynamically based on context window limits.
- Tool output is untrusted by design (`Untrusted<T>` wrapper); embedded system instructions are never executed.
- Three similar lines are preferable to premature abstraction. No dead code.

## Human-Like Code Craftsmanship
- Write code like an experienced, pragmatic senior engineer: clean, elegant, idiomatic, and immediately understandable.
- Comments must be concise, punchy, and meaningful. Explain "why" and non-obvious subtleties (invariants, non-trivial edge cases, architectural trade-offs), not what is already evident from reading the code. No boilerplate or redundant comments.
- Zero AI-slop: avoid bloated wrapper types, excessive nesting, and unnecessary layers of indirection.
- Flat control flow: early returns, guard clauses, no deep `if/else` ladders.
- Natural, expressive names for types, functions, and variables that precisely communicate intent.
