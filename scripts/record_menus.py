#!/usr/bin/env python3
import os
import subprocess
import time
import sys

def main():
    cast_path = "/tmp/menus.cast"
    gif_path = os.path.abspath("docs/screenshots/gifs/menus.gif")

    subprocess.run(["tmux", "kill-session", "-t", "demo-menu"], stderr=subprocess.DEVNULL)
    time.sleep(0.5)

    cols = 126
    rows = 28

    if os.path.exists(cast_path):
        os.remove(cast_path)

    cmd = f"asciinema rec -f asciicast-v2 {cast_path} -c ./target/release/flashagent"
    print(f"Launching tmux session for menus ({cols}x{rows})...")
    subprocess.run(["tmux", "new-session", "-d", "-s", "demo-menu", "-x", str(cols), "-y", str(rows), cmd], check=True)

    # Wait for startup
    print("Waiting for startup...")
    for _ in range(20):
        time.sleep(0.4)
        out = subprocess.run(["tmux", "capture-pane", "-t", "demo-menu", "-p"], capture_output=True, text=True).stdout
        if "FlashAgent" in out or "Welcome" in out:
            break

    time.sleep(1.8)

    # 1. Open F3 Model Menu
    print("Opening F3 Model selector...")
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "F3"])
    time.sleep(1.2)
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "Down"])
    time.sleep(0.6)
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "Down"])
    time.sleep(0.6)
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "Up"])
    time.sleep(0.6)
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "Enter"])
    time.sleep(1.0)

    # 2. Open F4 Thinking Effort Menu
    print("Opening F4 Thinking Effort...")
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "F4"])
    time.sleep(1.2)
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "Down"])
    time.sleep(0.5)
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "Enter"])
    time.sleep(1.0)

    # 3. Open F5 Sampling Parameters
    print("Opening F5 Sampling parameters...")
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "F5"])
    time.sleep(1.5)
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "Escape"])
    time.sleep(1.0)

    # 4. Open Tab Settings
    print("Opening Tab Settings...")
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "Tab"])
    time.sleep(1.5)
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "Down"])
    time.sleep(0.5)
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "Down"])
    time.sleep(0.5)
    subprocess.run(["tmux", "send-keys", "-t", "demo-menu", "Escape"])
    time.sleep(2.0)

    # Stop asciinema cleanly on final screen
    print("Stopping asciinema cleanly on final screen...")
    pane_pid = subprocess.run(["tmux", "list-panes", "-t", "demo-menu", "-F", "#{pane_pid}"], capture_output=True, text=True).stdout.strip()
    if pane_pid:
        subprocess.run(["kill", "-INT", pane_pid])
        time.sleep(1.0)
    subprocess.run(["tmux", "kill-session", "-t", "demo-menu"], stderr=subprocess.DEVNULL)

    # Render with agg
    print(f"Rendering menus.gif to {gif_path}...")
    agg_cmd = [
        "agg",
        "--theme", "dracula",
        "--font-size", "14",
        "--line-height", "1.3",
        "--speed", "1.3",
        "--idle-time-limit", "1.0",
        "--fps-cap", "25",
        "--last-frame-duration", "3",
        "--select", "0.8..",
        cast_path,
        gif_path
    ]
    subprocess.run(agg_cmd, check=True)
    size_kb = os.path.getsize(gif_path) / 1024
    print(f"menus.gif created: {gif_path} ({size_kb:.1f} KB)")

if __name__ == "__main__":
    main()
