# Rule: Build Versioning Discipline

## Beta Channel Build Increment Steps
When preparing releases on the Beta track (`b<number>`), every change MUST use the strict quantitative step system:

| Change Category | Build Increment | Examples |
|---|---|---|
| **Mini Bugfix** | `+1` | Typo, padding, label fix, visual alignment, single-line correction |
| **Cool / Handy Minor Feature** | `+3` | Shortcut, clipboard toast, indicator, token speed counter, flag |
| **Medium Bugfix** | `+5` | Input registration latency, tool failure recovery, terminal restore |
| **Big Bugfix / Architecture Fix** | `+10` | IPC protocol repair, memory leak, updater transition resilience |
| **Huge New Feature / Milestone** | `+15` | Major subsystem completion (MCP client/market, Updater, full track) |

## Cumulative Combining Rules
- When multiple changes are combined in a single working session or release, **sum the individual increments**.
- **Hard Cap**: The maximum increment for any single release or session is capped at **`+15`** (e.g. `+10` big fix + `+3` feature + `+1` mini fix = `+14`; `+10` + `+10` = `+15`).

## Hotfix Policy
- Always increment to the next whole integer build number according to the scale (e.g. mini hotfix to `b200` becomes `b201`).
- Suffixes like `-hotfix1`, `-patch1`, or `.1` are **strictly forbidden** on the beta channel.

## Transition to Stable (v1.0.0)
- The beta track continues until full completion and verification of **Tracks A and C**. (Track B is scheduled for v2.0.0).
- Release `v1.0.0` (Stable) will occur **ONLY** after proven, battle-tested stability of a beta build with 100% completed functionality and roadmap plan.
- In Stable releases, default update channel is `Stable`. In Beta builds, default update channel is `Beta`.
