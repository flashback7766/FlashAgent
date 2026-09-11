#!/usr/bin/env python3
import os
import subprocess
import time
import sys

def main():
    cast_path = "/tmp/tools.cast"
    gif_path = os.path.abspath("docs/screenshots/gifs/tools-diff.gif")

    subprocess.run(["tmux", "kill-session", "-t", "demo-tools"], stderr=subprocess.DEVNULL)
    time.sleep(0.5)

    cols = 126
    rows = 28

    if os.path.exists(cast_path):
        os.remove(cast_path)

    cmd = f"asciinema rec -f asciicast-v2 {cast_path} -c ./target/release/flashagent"
    print(f"Launching tmux session for tools ({cols}x{rows})...")
    subprocess.run(["tmux", "new-session", "-d", "-s", "demo-tools", "-x", str(cols), "-y", str(rows), cmd], check=True)

    # Wait for startup
    print("Waiting for startup...")
    for _ in range(20):
        time.sleep(0.4)
        out = subprocess.run(["tmux", "capture-pane", "-t", "demo-tools", "-p"], capture_output=True, text=True).stdout
        if "FlashAgent" in out or "Welcome" in out:
            break

    time.sleep(1.8)

    prompt = "Read Cargo.toml and list workspace members"
    print(f"Typing prompt: {prompt}")
    for char in prompt:
        subprocess.run(["tmux", "send-keys", "-t", "demo-tools", "-l", char])
        time.sleep(0.035)

    time.sleep(0.6)
    subprocess.run(["tmux", "send-keys", "-t", "demo-tools", "Enter"])
    print("Sent Enter, monitoring tool execution...")

    start_time = time.time()
    seen_generation = False
    idle_count = 0

    while time.time() - start_time < 50:
        time.sleep(0.6)
        out = subprocess.run(["tmux", "capture-pane", "-t", "demo-tools", "-p"], capture_output=True, text=True).stdout
        
        is_generating = ("Prefill" in out or "Tokens -" in out or "Working" in out)
        if is_generating:
            seen_generation = True
            idle_count = 0
            for line in out.splitlines():
                if "Tokens -" in line or "Prefill" in line or "Tool" in line:
                    print(f"  [streaming] {line.strip()[:70]}")
                    break
        elif seen_generation:
            idle_count += 1
            if idle_count >= 5:
                print("Generation complete!")
                break

    time.sleep(3.5)
    print("Stopping asciinema cleanly on completed screen...")
    pane_pid = subprocess.run(["tmux", "list-panes", "-t", "demo-tools", "-F", "#{pane_pid}"], capture_output=True, text=True).stdout.strip()
    if pane_pid:
        subprocess.run(["kill", "-INT", pane_pid])
        time.sleep(1.0)
    subprocess.run(["tmux", "kill-session", "-t", "demo-tools"], stderr=subprocess.DEVNULL)

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
