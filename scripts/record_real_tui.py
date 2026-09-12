#!/usr/bin/env python3
import os
import subprocess
import time
import sys

def main():
    cast_path = "/tmp/flashagent_real.cast"
    gif_path = os.path.abspath("docs/screenshots/gifs/hero-chat.gif")

    # 1. Clean up any previous session
    subprocess.run(["tmux", "kill-session", "-t", "demo"], stderr=subprocess.DEVNULL)
    time.sleep(0.5)

    cols = 126
    rows = 28

    if os.path.exists(cast_path):
        os.remove(cast_path)

    cmd_str = f"asciinema rec -f asciicast-v2 --overwrite {cast_path} -c ./target/release/flashagent"
    print(f"Launching tmux session with asciinema ({cols}x{rows})...")
    subprocess.run(["tmux", "new-session", "-d", "-s", "demo", "-x", str(cols), "-y", str(rows), cmd_str], check=True)

    # 2. Wait for startup screen
    print("Waiting for FlashAgent to initialize...")
    started = False
    for _ in range(20):
        time.sleep(0.5)
        out = subprocess.run(["tmux", "capture-pane", "-t", "demo", "-p"], capture_output=True, text=True).stdout
        if "FlashAgent" in out or "Welcome" in out:
            started = True
            break
    if not started:
        print("Failed to detect FlashAgent startup!")
        sys.exit(1)

    print("FlashAgent started cleanly. Pausing on welcome card...")
    time.sleep(2.0)

    # 3. Type prompt naturally
    prompt = "Explain mid-flight steering in FlashAgent in 2 sentences"
    print(f"Typing prompt: {prompt}")
    for char in prompt:
        subprocess.run(["tmux", "send-keys", "-t", "demo", "-l", char])
        time.sleep(0.035)

    time.sleep(0.6)
    subprocess.run(["tmux", "send-keys", "-t", "demo", "Enter"])
    print("Sent Enter, monitoring streaming generation...")

    # 4. Monitor until completion
    start_time = time.time()
    seen_generation = False
    idle_count = 0

    while time.time() - start_time < 45:
        time.sleep(0.6)
        out = subprocess.run(["tmux", "capture-pane", "-t", "demo", "-p"], capture_output=True, text=True).stdout
        
        is_generating = ("Prefill" in out or "Tokens -" in out or "Working on task" in out)
        if is_generating:
            seen_generation = True
            idle_count = 0
            for line in out.splitlines():
                if "Tokens -" in line or "Prefill" in line:
                    print(f"  [streaming] {line.strip()[:70]}")
                    break
        elif seen_generation:
            idle_count += 1
            if idle_count >= 3:
                print("Generation complete!")
                break

    # 5. Wait for live recap and suggestion to appear from background task
    print("Waiting for recap and ghost suggestion...")
    for _ in range(25):
        time.sleep(0.4)
        out = subprocess.run(["tmux", "capture-pane", "-t", "demo", "-p"], capture_output=True, text=True).stdout
        if "recap:" in out or "(→ to use)" in out:
            print("Detected live recap and ghost suggestion!")
            break

    # Pause 2 seconds so viewer clearly reads the recap and sees the ghost suggestion in composer
    time.sleep(2.0)

    # Accept ghost suggestion with Right arrow!
    print("Pressing Right arrow (→) to accept ghost suggestion...")
    subprocess.run(["tmux", "send-keys", "-t", "demo", "Right"])
    time.sleep(2.5)

    # 6. Stop asciinema cleanly with SIGINT before exiting TUI
    print("Stopping asciinema cleanly on completed screen...")
    pane_pid = subprocess.run(["tmux", "list-panes", "-t", "demo", "-F", "#{pane_pid}"], capture_output=True, text=True).stdout.strip()
    if pane_pid:
        subprocess.run(["kill", "-INT", pane_pid])
        time.sleep(1.0)

    subprocess.run(["tmux", "kill-session", "-t", "demo"], stderr=subprocess.DEVNULL)

    if not os.path.exists(cast_path):
        print(f"Error: {cast_path} was not created!")
        sys.exit(1)

    print(f"Cast recording saved ({os.path.getsize(cast_path)} bytes).")

    # 7. Render with agg
    print(f"Rendering GIF with agg to {gif_path}...")
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
    print(f"Done! Real GIF saved to {gif_path} ({size_kb:.1f} KB)")


if __name__ == "__main__":
    main()
