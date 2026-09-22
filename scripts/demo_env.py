"""What the README recordings share: a throwaway home, tmux, asciinema, agg.

The GIFs are public, so they must not show the recorder's own sessions,
memory or prompt history. Each recording runs with HOME pointed at a fresh
folder that holds only a config for the demo model.

    FLASHAGENT_DEMO_MODEL    model id on the server (default: what LM Studio has loaded)
    FLASHAGENT_DEMO_BACKEND  OpenAI-compatible URL (default http://localhost:1234/v1)

Needs tmux, asciinema, agg and a release build (cargo build --release).
"""

import json
import os
import shutil
import subprocess
import sys
import time
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(REPO, "target", "release", "flashagent")
GIFS = os.path.join(REPO, "docs", "screenshots", "gifs")
BACKEND = os.environ.get("FLASHAGENT_DEMO_BACKEND", "http://localhost:1234/v1")
COLS, ROWS = 120, 30


def loaded_model():
    """The model LM Studio has loaded, so a recording never makes it load another."""
    if os.environ.get("FLASHAGENT_DEMO_MODEL"):
        return os.environ["FLASHAGENT_DEMO_MODEL"]
    base = BACKEND.rsplit("/v1", 1)[0]
    with urllib.request.urlopen(f"{base}/api/v0/models", timeout=5) as r:
        models = json.load(r)["data"]
    loaded = [m["id"] for m in models if m.get("state") == "loaded" and m.get("type") == "llm"]
    if not loaded:
        sys.exit("No model is loaded in LM Studio; load one or set FLASHAGENT_DEMO_MODEL.")
    return loaded[0]


class Demo:
    def __init__(self, name, cols=COLS, rows=ROWS, **config):
        self.name = name
        self.session = f"fa-demo-{name}"
        self.cols, self.rows = cols, rows
        self.root = os.path.join("/tmp", f"flashagent-demo-{name}")
        shutil.rmtree(self.root, ignore_errors=True)
        self.home = os.path.join(self.root, "home")
        self.cast = os.path.join(self.root, f"{name}.cast")
        os.makedirs(os.path.join(self.home, ".flashagent"))
        # The token line reads LM Studio's own log for the cache figure.
        logs = os.path.expanduser("~/.lmstudio/server-logs")
        if os.path.isdir(logs):
            os.makedirs(os.path.join(self.home, ".lmstudio"))
            os.symlink(logs, os.path.join(self.home, ".lmstudio", "server-logs"))
        self.config = {
            "backend_url": BACKEND,
            "model": loaded_model(),
            "setup_completed": True,
            "permission_mode": "AcceptEdits",
            "thinking_effort": "auto",
            "auto_check_updates": False,
            "last_seen_version": os.environ.get("FLASHAGENT_VERSION", "b330"),
            "trusted_directories": [],
        }
        self.config.update(config)
        self.started = None

    def project(self, name="demo-project", files=None, clone=None):
        """A working folder inside the demo home: given files, or a clone of a repo."""
        path = os.path.join(self.home, name)
        if clone:
            subprocess.run(["git", "clone", "-q", clone, path], check=True)
        else:
            os.makedirs(path)
            for rel, text in (files or {}).items():
                os.makedirs(os.path.dirname(os.path.join(path, rel)), exist_ok=True)
                with open(os.path.join(path, rel), "w") as f:
                    f.write(text)
            subprocess.run(["git", "init", "-q"], cwd=path)
            subprocess.run(["git", "add", "."], cwd=path)
            subprocess.run(["git", "-c", "user.name=demo", "-c", "user.email=demo@example.com",
                            "commit", "-qm", "start"], cwd=path)
        self.config["trusted_directories"].append(path)
        return path

    def start(self, cwd):
        with open(os.path.join(self.home, ".flashagent", "config.json"), "w") as f:
            json.dump(self.config, f, indent=2)
        subprocess.run(["tmux", "kill-session", "-t", self.session], stderr=subprocess.DEVNULL)
        env = dict(os.environ, HOME=self.home, TERM="xterm-256color", COLORTERM="truecolor")
        env.pop("FLASHAGENT_CONFIG_PATH", None)
        # The welcome card greets $USER; a public recording greets nobody in particular.
        for var in ("USER", "USERNAME", "LOGNAME"):
            env.pop(var, None)
        cmd = f"asciinema rec -q -f asciicast-v2 --overwrite {self.cast} -c {BIN}"
        subprocess.run(["tmux", "new-session", "-d", "-s", self.session, "-c", cwd,
                        "-x", str(self.cols), "-y", str(self.rows), cmd], check=True, env=env)
        self.started = time.time()
        self.wait_for("FlashAgent", 20)

    def screen(self):
        return subprocess.run(["tmux", "capture-pane", "-t", self.session, "-p"],
                              capture_output=True, text=True).stdout

    def wait_for(self, text, timeout):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if text in self.screen():
                return True
            time.sleep(0.25)
        print(f"[{self.name}] not on screen after {timeout}s: {text!r}\n{self.screen()}")
        return False

    def wait_idle(self, timeout, quiet=3.0):
        """Until the screen stops changing for `quiet` seconds."""
        deadline = time.time() + timeout
        last, since = None, time.time()
        while time.time() < deadline:
            now = self.screen()
            if now != last:
                last, since = now, time.time()
            elif time.time() - since >= quiet:
                return
            time.sleep(0.3)

    def wait_turn(self, timeout):
        """Until the turn is over: the working face is gone from the footer."""
        time.sleep(1.5)
        deadline = time.time() + timeout
        while time.time() < deadline:
            s = self.screen()
            if "esc to interrupt" not in s.lower() and "Interrupting" not in s:
                return True
            time.sleep(0.4)
        return False

    def type(self, text, per_char=0.04):
        for ch in text:
            subprocess.run(["tmux", "send-keys", "-t", self.session, "-l", ch])
            time.sleep(per_char)

    def key(self, *keys, pause=0.6):
        for k in keys:
            subprocess.run(["tmux", "send-keys", "-t", self.session, k])
            time.sleep(pause)

    def stop(self):
        pane = subprocess.run(["tmux", "list-panes", "-t", self.session, "-F", "#{pane_pid}"],
                              capture_output=True, text=True).stdout.strip()
        if pane:
            subprocess.run(["kill", "-INT", pane])
            time.sleep(1.0)
        subprocess.run(["tmux", "kill-session", "-t", self.session], stderr=subprocess.DEVNULL)

    def cast_time(self, text, lead=1.5):
        """Where the GIF starts: `lead` seconds before `text` first appears.

        Wall-clock time will not do: asciinema shortens long pauses, so the
        cast is shorter than the recording took.
        """
        if not text:
            return 0.0
        with open(self.cast) as f:
            next(f)
            for line in f:
                t, kind, data = json.loads(line)
                if kind == "o" and text in data:
                    return max(t - lead, 0.0)
        return 0.0

    def render(self, gif, start_text=None, speed=1.5, idle=1.2):
        """The cast from `start_text` on, as a GIF. What came before is
        drawn at once, so the first frame is the screen as it stood."""
        out = os.path.join(GIFS, gif)
        start = self.cast_time(start_text)
        trimmed = self.cast + ".trimmed"
        with open(self.cast) as src, open(trimmed, "w") as dst:
            dst.write(next(src))
            for line in src:
                t, kind, data = json.loads(line)
                dst.write(json.dumps([max(t - start, 0.0), kind, data]) + "\n")
        subprocess.run(["agg", "--quiet", "--theme", "dracula", "--font-size", "14", "--line-height", "1.3",
                        "--speed", str(speed), "--idle-time-limit", str(idle), "--fps-cap", "20",
                        "--last-frame-duration", "3", trimmed, out], check=True)
        print(f"[{self.name}] {out} ({os.path.getsize(out) // 1024} KB)")

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)
