#!/usr/bin/env python3
"""tools-diff.gif: an edit waits on its diff until it is allowed."""
from demo_env import Demo

PARSER = '''use std::time::Duration;

pub fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    if let Some(s) = text.strip_suffix('s') {
        return s.parse().ok().map(Duration::from_secs);
    }
    text.parse::<u64>().ok().map(|m| Duration::from_secs(m * 60))
}
'''

d = Demo("tools", permission_mode="Manual")
p = d.project("parser-demo", files={"src/parser.rs": PARSER, "Cargo.toml": '[package]\nname = "parser-demo"\nversion = "0.1.0"\nedition = "2021"\n'})
d.start(p)
d.wait_idle(120, quiet=6)
d.type("Add the doc comment /// A bare number means minutes. above parse_duration in src/parser.rs")
d.key("Enter", pause=0)
if d.wait_for("Esc deny", 300):
    d.wait_idle(5, quiet=2.5)
    d.key("Enter", pause=0)
d.wait_turn(300)
d.wait_idle(15, quiet=3)
d.stop()
d.render("tools-diff.gif", start_text="Add the d")
d.cleanup()
