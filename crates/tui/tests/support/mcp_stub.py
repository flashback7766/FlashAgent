"""A stand-in MCP server over stdio, for the scenario tests.

Two tools: `lookup`, which only reads, and `delete_everything`, which does
not. Every call is appended to the file named by MCP_STUB_LOG, as is the
moment the client lists the tools, so a scenario can wait for the server to
be up and can tell whether a call ever reached it.

Python rather than a shell script so it runs the same on Linux, macOS and
Windows.
"""

import json
import os
import sys

LOG = os.environ.get("MCP_STUB_LOG")


def log(line):
    if LOG:
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(line + "\n")


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


TOOLS = [
    {
        "name": "lookup",
        "description": "Look up the value stored under a key.",
        "inputSchema": {"type": "object", "properties": {"key": {"type": "string"}}, "required": ["key"]},
    },
    {
        "name": "delete_everything",
        "description": "Delete every stored value.",
        "inputSchema": {"type": "object", "properties": {}},
    },
]

for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    message = json.loads(raw)
    request_id = message.get("id")
    method = message.get("method")
    if request_id is None:
        # A notification, such as notifications/initialized: no reply.
        continue
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": request_id, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "stub", "version": "1"},
        }})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"tools": TOOLS}})
        log("listed")
    elif method == "tools/call":
        params = message.get("params") or {}
        name = params.get("name")
        arguments = params.get("arguments") or {}
        log("call " + str(name))
        if name == "lookup":
            text = "value for " + str(arguments.get("key"))
        else:
            text = "everything deleted"
        send({"jsonrpc": "2.0", "id": request_id, "result": {
            "content": [{"type": "text", "text": text}],
            "isError": False,
        }})
    else:
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
