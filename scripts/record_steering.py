#!/usr/bin/env python3
import os
import subprocess
import time
import sys

def main():
    cast_path = "/tmp/steering.cast"
    gif_path = os.path.abspath("docs/screenshots/gifs/steering.gif")

    subprocess.run(["tmux", "kill-session", "-t", "demo-steer"], stderr=subprocess.DEVNULL)
    time.sleep(0.5)

    cols = 126
    rows = 28

    if os.path.exists(cast_path):
        os.remove(cast_path)

    cmd = f"asciinema rec -f asciicast-v2 {cast_path} -c ./target/release/flashagent-tui"
    print(f"Launching tmux session for steering ({cols}x{rows})...")
    subprocess.run(["tmux", "new-session", "-d", "-s", "demo-steer", "-x", str(cols), "-y", str(rows), cmd], check=True)

    # Wait for startup
    for _ in range(20):
        time.sleep(0.4)
        out = subprocess.run(["tmux", "capture-pane", "-t", "demo-steer", "-p"], capture_output=True, text=True).stdout
        if "FlashAgent" in out or "Welcome" in out:
            break

    time.sleep(1.8)

    prompt = "Write a 300-word essay about space colonization"
    print(f"Typing initial prompt: {prompt}")
    for char in prompt:
        subprocess.run(["tmux", "send-keys", "-t", "demo-steer", "-l", char])
        time.sleep(0.03)

    time.sleep(0.5)
    subprocess.run(["tmux", "send-keys", "-t", "demo-steer", "Enter"])
    print("Sent initial prompt. Waiting for streaming tokens...")

    # Wait until generation is actively streaming tokens
    for _ in range(30):
        time.sleep(0.5)
        out = subprocess.run(["tmux", "capture-pane", "-t", "demo-steer", "-p"], capture_output=True, text=True).stdout
        if "Tokens -" in out:
            print("Detected active streaming!")
            break

    # Let it stream a little bit (~2 seconds)
    time.sleep(2.0)

    # Now type mid-flight steering directive!
    steer = "Stop. Instead explain mid-flight steering in 1 sentence."
    print(f"Injecting mid-flight steering: {steer}")
    for char in steer:
        subprocess.run(["tmux", "send-keys", "-t", "demo-steer", "-l", char])
        time.sleep(0.03)

    time.sleep(0.4)
    subprocess.run(["tmux", "send-keys", "-t", "demo-steer", "Enter"])
    print("Sent steering directive! Monitoring continuation...")

    # Wait until generation finishes
    seen_steer_done = False
    start_t = time.time()
    while time.time() - start_t < 40:
        time.sleep(0.6)
        out = subprocess.run(["tmux", "capture-pane", "-t", "demo-steer", "-p"], capture_output=True, text=True).stdout
        if "Tokens -" in out or "Working" in out:
            seen_steer_done = True
        elif seen_steer_done:
            # Idle
            print("Steered generation complete!")
            break

    time.sleep(3.5)
    print("Stopping asciinema cleanly on completed screen...")
    pane_pid = subprocess.run(["tmux", "list-panes", "-t", "demo-steer", "-F", "#{pane_pid}"], capture_output=True, text=True).stdout.strip()
    if pane_pid:
        subprocess.run(["kill", "-INT", pane_pid])
        time.sleep(1.0)
    subprocess.run(["tmux", "kill-session", "-t", "demo-steer"], stderr=subprocess.DEVNULL)

    print(f"Rendering steering.gif to {gif_path}...")
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
    print(f"steering.gif created: {gif_path} ({size_kb:.1f} KB)")

if __name__ == "__main__":
    main()
