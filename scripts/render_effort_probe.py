#!/usr/bin/env python3
"""Ask llama-server to render the same turn at two effort levels and report where
the rendered text stops agreeing.

`reasoning_effort` reaches the jinja chat template (tools/server/server-chat.cpp:290).
If the template puts it into the prompt, switching effort moves the prompt-cache
boundary and costs a re-prefill of everything after that point - measured at 12.5 s
after switching max -> medium in the TUI. This probe separates "the template
changed" from "something else invalidated the cache".

    PORT=8034 ./scripts/render_effort_probe.py
"""
import json
import os
import sys
import urllib.request

URL = f"http://127.0.0.1:{os.environ.get('PORT', '8034')}"


def render(effort, endpoint):
    body = json.dumps({
        "model": "probe",
        "messages": [
            {"role": "system", "content": "You are FlashAgent.\n- Be concise."},
            {"role": "user", "content": "Say hi."},
        ],
        "reasoning_effort": effort,
        "stream": False,
        "max_tokens": 1,
    }).encode()
    req = urllib.request.Request(f"{URL}{endpoint}", data=body,
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.read().decode("utf-8", "replace")
    except Exception as exc:
        return f"__ERROR__ {exc}"


def common_prefix(a, b):
    n = min(len(a), len(b))
    i = 0
    while i < n and a[i] == b[i]:
        i += 1
    return i


def main():
    endpoint = sys.argv[1] if len(sys.argv) > 1 else "/apply-template"
    out = {}
    for effort in ("max", "medium"):
        r = render(effort, endpoint)
        if r.startswith("__ERROR__"):
            print(f"{endpoint} unavailable: {r[:160]}")
            print("falling back: cannot probe the template from here.")
            return 1
        out[effort] = r
        print(f"{effort:7s} {len(r):5d} chars  {r[:110]!r}")

    a, b = out["max"], out["medium"]
    p = common_prefix(a, b)
    print()
    print(f"common prefix: {p} chars of {min(len(a), len(b))}")
    if p == len(a) == len(b):
        print("IDENTICAL: reasoning_effort does not change the rendered prompt.")
        print("The 12.5 s after switching effort came from somewhere else.")
    else:
        print("DIFFERENT: the template renders effort into the prompt, so switching")
        print("it moves the cache boundary and everything after it is re-prefilled.")
        print(f"first difference at {p}:")
        print(f"  max    ...{a[max(0, p - 40):p + 60]!r}")
        print(f"  medium ...{b[max(0, p - 40):p + 60]!r}")
    return 0


if __name__ == "__main__":
    sys.exit(main())