#!/usr/bin/env python3
"""menus.gif: the model, effort, sampling and settings menus, none of them blocking."""
from demo_env import Demo

d = Demo("menus")
p = d.project(files={"README.md": "# notes\n"})
d.start(p)
d.wait_idle(120, quiet=6)
# The menus open while an answer is streaming: none of them stops it.
d.type("Write a 300-word story about a lighthouse keeper")
d.key("Enter", pause=0)
d.wait_for("Tokens -", 180)
d.key("F3", pause=1.2)
d.key("Down", "Up", pause=0.7)
d.key("Escape", pause=1.0)
d.key("F4", pause=1.2)
d.key("Down", "Down", pause=0.7)
d.key("Escape", pause=1.0)
d.key("F5", pause=1.5)
d.key("Escape", pause=1.0)
d.key("Tab", pause=1.5)
d.key("3", "Down", "Down", pause=0.9)  # the UI tab
d.key("7", "Down", pause=0.9)  # the voice

d.key("Escape", pause=1.5)
d.wait_turn(300)
d.wait_idle(10, quiet=2)
d.stop()
d.render("menus.gif", start_text="Write a 3", speed=1.2, idle=2.0)
d.cleanup()
