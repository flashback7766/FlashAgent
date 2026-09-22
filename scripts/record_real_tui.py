#!/usr/bin/env python3
"""hero-chat.gif: a question about a project, answered from its files."""
from demo_env import Demo

TASKS_PY = '''"""A to-do list kept in a JSON file next to you."""
import json
import sys
from pathlib import Path

STORE = Path("tasks.json")


def load():
    return json.loads(STORE.read_text()) if STORE.exists() else []


def save(tasks):
    STORE.write_text(json.dumps(tasks, indent=2))


def main(argv):
    tasks = load()
    if argv[:1] == ["add"]:
        tasks.append({"text": " ".join(argv[1:]), "done": False})
    elif argv[:1] == ["done"]:
        tasks[int(argv[1]) - 1]["done"] = True
    for i, t in enumerate(tasks, 1):
        print(f"{i}. [{'x' if t['done'] else ' '}] {t['text']}")
    save(tasks)


if __name__ == "__main__":
    main(sys.argv[1:])
'''

d = Demo("hero")
p = d.project("tasks", files={
    "tasks.py": TASKS_PY,
    "README.md": "# tasks\n\nA tiny command-line to-do list.\n\n    python tasks.py add buy milk\n    python tasks.py done 1\n",
    "test_tasks.py": "from tasks import main\n\ndef test_add(tmp_path, monkeypatch):\n    monkeypatch.chdir(tmp_path)\n    main(['add', 'x'])\n",
})
d.start(p)
d.wait_idle(120, quiet=6)  # the welcome card, and the warm-up behind it
d.type("What does this project do, and where would I start reading?")
d.key("Enter", pause=0)
d.wait_turn(300)
d.wait_for("(→", 60)  # the recap and the suggested next prompt
d.wait_idle(10, quiet=2)
d.key("Right", pause=2.5)
d.stop()
d.render("hero-chat.gif", start_text="What does")
d.cleanup()
