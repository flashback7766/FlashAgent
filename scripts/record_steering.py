#!/usr/bin/env python3
"""steering.gif: a message typed while the model writes changes where it goes."""
import time
from demo_env import Demo

d = Demo("steering")
p = d.project(files={"README.md": "# notes\n"})
d.start(p)
d.wait_idle(120, quiet=6)
d.type("Write a 300-word essay about space colonization")
d.key("Enter", pause=0)
d.wait_for("t/s", 180)
time.sleep(4)
d.type("focus on Mars only, and end with one open question")
d.key("Enter", pause=0)
d.wait_turn(300)
d.wait_idle(15, quiet=3)
d.stop()
d.render("steering.gif", start_text="Write a 3", speed=2.0)
d.cleanup()
