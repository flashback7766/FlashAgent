#!/usr/bin/env python3
import os
import subprocess
import time
import sys
import shutil
import json

def main():
    cast_path = "/tmp/tools.cast"
    gif_path = os.path.abspath("docs/screenshots/gifs/tools-diff.gif")
    workdir = "/tmp/parser-demo"

    if os.path.exists(workdir):
        shutil.rmtree(workdir)
    os.makedirs(os.path.join(workdir, "src"), exist_ok=True)
    with open(os.path.join(workdir, "src", "parser.rs"), "w") as f:
        f.write("fn a() {}\n\npub fn parse_duration() {}\n")

    subprocess.run(["tmux", "kill-session", "-t", "demo-tools"], stderr=subprocess.DEVNULL)
    time.sleep(0.5)

    cols = 126
    rows = 28

    if os.path.exists(cast_path):
        os.remove(cast_path)

    cfg_path = "/tmp/fa_tools_cfg.json"
    with open(os.path.expanduser("~/.flashagent/config.json")) as f:
        cfg = json.load(f)
    cfg["permission_mode"] = "Manual"
    with open(cfg_path, "w") as f:
        json.dump(cfg, f)

    bin_path = os.path.abspath("./target/release/flashagent")
    cmd = f"asciinema rec -f asciicast-v2 {cast_path} -c {bin_path}"
    print(f"Launching tmux session for tools in {workdir} ({cols}x{rows})...")
    env = dict(os.environ, FLASHAGENT_TRUST_DIR="1", FLASHAGENT_CONFIG_PATH=cfg_path)
    subprocess.run(["tmux", "new-session", "-d", "-s", "demo-tools", "-c", workdir, "-x", str(cols), "-y", str(rows), cmd], check=True, env=env)

    # Wait for startup
    print("Waiting for startup...")
    for _ in range(25):
        time.sleep(0.4)
        out = subprocess.run(["tmux", "capture-pane", "-t", "demo-tools", "-p"], capture_output=True, text=True).stdout
        if "FlashAgent" in out or "Welcome" in out:
            break

    time.sleep(1.8)

    prompt = "Add doc comment /// A bare number means minutes. before pub fn parse_duration() in src/parser.rs"
    print(f"Typing prompt: {prompt}")
    for char in prompt:
        subprocess.run(["tmux", "send-keys", "-t", "demo-tools", "-l", char])
        time.sleep(0.025)

    time.sleep(0.6)
    subprocess.run(["tmux", "send-keys", "-t", "demo-tools", "Enter"])
    print("Sent Enter, monitoring tool execution and diff approval card...")

    start_time = time.time()
    approved = False
    seen_edit = False

    while time.time() - start_time < 90:
        time.sleep(0.6)
        out = subprocess.run(["tmux", "capture-pane", "-t", "demo-tools", "-p"], capture_output=True, text=True).stdout
        
        # Check if approval card appeared
        if ("Allow" in out or "Deny" in out or "Confirm:" in out) and not approved:
            print("Detected diff approval card! Pausing for viewer...")
            time.sleep(2.5)
            print("Sending Enter to Allow edit...")
            subprocess.run(["tmux", "send-keys", "-t", "demo-tools", "Enter"])
            approved = True
            time.sleep(1.5)
            continue

        if ("Edited" in out or "Edited parser.rs" in out) and approved:
            seen_edit = True

        if seen_edit and not ("Tokens -" in out or "Working" in out or "Prefill" in out):
            print("Tool execution and diff approved cleanly!")
            break

    time.sleep(3.5)
    print("Stopping asciinema cleanly on completed screen...")
    pane_pid = subprocess.run(["tmux", "list-panes", "-t", "demo-tools", "-F", "#{pane_pid}"], capture_output=True, text=True).stdout.strip()
    if pane_pid:
        subprocess.run(["kill", "-INT", pane_pid])
        time.sleep(1.0)
    subprocess.run(["tmux", "kill-session", "-t", "demo-tools"], stderr=subprocess.DEVNULL)

    if os.path.exists(workdir):
        shutil.rmtree(workdir, ignore_errors=True)

    print(f"Rendering tools-diff.gif to {gif_path}...")
    agg_cmd = [
        "agg",
        "--theme", "dracula",
        "--font-size", "14",
        "--line-height", "1.3",
        "--speed", "1.6",
        "--idle-time-limit", "1.2",
        "--fps-cap", "25",
        "--last-frame-duration", "3",
        "--select", "0.8..",
        cast_path,
        gif_path
    ]
    subprocess.run(agg_cmd, check=True)
    size_kb = os.path.getsize(gif_path) / 1024
    print(f"tools-diff.gif created: {gif_path} ({size_kb:.1f} KB)")

if __name__ == "__main__":
    main()
