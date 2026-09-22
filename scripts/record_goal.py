#!/usr/bin/env python3
"""goal-budget.gif: a /goal run stopped by its step budget, with the report."""
from demo_env import Demo

d = Demo("goal", goal_max_steps=3, permission_mode="AcceptEdits")
p = d.project("goal-demo", files={
    "src/lib.rs": "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    "Cargo.toml": '[package]\nname = "goal-demo"\nversion = "0.1.0"\nedition = "2021"\n',
})
d.start(p)
d.wait_idle(120, quiet=6)
d.type("/goal add a doc comment to add() in src/lib.rs, create NOTES.md with one line about this crate, then run cargo check")
d.key("Enter", pause=0)
d.wait_turn(600)
d.wait_idle(15, quiet=3)
d.stop()
d.render("goal-budget.gif", start_text="/goal add", speed=2.0)
d.cleanup()
