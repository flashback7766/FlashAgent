#!/usr/bin/env python3
import os
import subprocess
import time
import sys
import shutil

def main():
    cast_path = "/tmp/goal.cast"
    gif_path = os.path.abspath("docs/screenshots/gifs/goal-budget.gif")
    workdir = "/tmp/fa_goal_demo"

    if os.path.exists(workdir):
        shutil.rmtree(workdir)
    os.makedirs(workdir, exist_ok=True)
    subprocess.run(["git", "init"], cwd=workdir, capture_output=True)

    subprocess.run(["tmux", "kill-session", "-t", "demo-goal"], stderr=subprocess.DEVNULL)
    time.sleep(0.5)

    cols = 126
    rows = 30

    if os.path.exists(cast_path):
        os.remove(cast_path)

    bin_path = os.path.abspath("./target/release/flashagent")
    cmd = f"asciinema rec -f asciicast-v2 {cast_path} -c {bin_path}"
    print(f"Launching tmux session for goal in {workdir} ({cols}x{rows})...")
    env = dict(os.environ, FLASHAGENT_TRUST_DIR="1", FLASHAGENT_CONFIG_PATH="/tmp/fa_goal_demo_config.json")
    subprocess.run(["tmux", "new-session", "-d", "-s", "demo-goal", "-c", workdir, "-x", str(cols), "-y", str(rows), cmd], check=True, env=env)

    # Wait for startup
    print("Waiting for startup...")
    for _ in range(25):
        time.sleep(0.4)
        out = subprocess.run(["tmux", "capture-pane", "-t", "demo-goal", "-p"], capture_output=True, text=True).stdout
        if "FlashAgent" in out or "Welcome" in out:
            break

    time.sleep(1.8)

    prompt = "/goal create NOTES.md with one line about this crate, then show it"
    print(f"Typing goal prompt: {prompt}")
    for char in prompt:
        subprocess.run(["tmux", "send-keys", "-t", "demo-goal", "-l", char])
        time.sleep(0.025)

    time.sleep(0.6)
    subprocess.run(["tmux", "send-keys", "-t", "demo-goal", "Enter"])
    print("Sent Enter, monitoring goal execution...")

    start_time = time.time()
    seen_goal_report = False

    while time.time() - start_time < 90:
        time.sleep(0.8)
        out = subprocess.run(["tmux", "capture-pane", "-t", "demo-goal", "-p"], capture_output=True, text=True).stdout
        if "Goal report" in out or "INCOMPLETE" in out or "budget reached" in out:
            seen_goal_report = True
            print("Detected goal report card!")
            break
        for line in out.splitlines():
            if "Step " in line or "step" in line or "Goal" in line:
                print(f"  [goal] {line.strip()[:70]}")
                break

    if not seen_goal_report:
        print("Warning: Goal report not detected within timeout, capturing whatever finished...")

    time.sleep(4.0)
    print("Stopping asciinema cleanly on completed report screen...")
    pane_pid = subprocess.run(["tmux", "list-panes", "-t", "demo-goal", "-F", "#{pane_pid}"], capture_output=True, text=True).stdout.strip()
    if pane_pid:
        subprocess.run(["kill", "-INT", pane_pid])
        time.sleep(1.0)
    subprocess.run(["tmux", "kill-session", "-t", "demo-goal"], stderr=subprocess.DEVNULL)

    if os.path.exists(workdir):
        shutil.rmtree(workdir, ignore_errors=True)

    print(f"Rendering goal-budget.gif to {gif_path}...")
    agg_cmd = [
        "agg",
        "--theme", "dracula",
        "--font-size", "14",
        "--line-height", "1.3",
        "--speed", "1.8",
        "--idle-time-limit", "1.2",
        "--fps-cap", "25",
        "--last-frame-duration", "4",
        "--select", "0.8..",
        cast_path,
        gif_path
    ]
    subprocess.run(agg_cmd, check=True)
    size_kb = os.path.getsize(gif_path) / 1024
    print(f"goal-budget.gif created: {gif_path} ({size_kb:.1f} KB)")

if __name__ == "__main__":
    main()
