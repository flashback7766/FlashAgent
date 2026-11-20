"""The serve client: event parsing, including the reply the model sends
with chat_reply. The transport is a mock, so the shapes are pinned."""

import asyncio
import json

import httpx

from flashagent_bot.brain import ServeClient


def make(body: bytes):
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=body, headers={"content-type": "application/json"})

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    return ServeClient("http://127.0.0.1:8094", "tok"), http


def test_poll_parses_the_reply_event():
    body = json.dumps(
        {
            "events": [
                {"type": "reply", "message_id": 42, "text": "держи ответ"},
                {"type": "text", "text": "хвост"},
                {"type": "tool_start", "name": "read"},
            ]
        }
    ).encode()
    client, http = make(body)
    try:
        events = asyncio.run(client.poll(http))
    finally:
        asyncio.run(http.aclose())
    reply = events[0]
    assert reply.type == "reply"
    assert reply.message_id == 42
    assert reply.text == "держи ответ"
    assert events[1].type == "text"
    assert events[2].type == "tool"


def test_since_advances_by_the_events_seen():
    body = json.dumps({"events": [{"type": "reply", "message_id": 1, "text": "x"}]}).encode()
    client, http = make(body)
    try:
        asyncio.run(client.poll(http))
        assert client.since == 1
    finally:
        asyncio.run(http.aclose())


def test_catch_up_returns_the_stale_events_for_a_late_delivery():
    body = json.dumps(
        {
            "events": [
                {"type": "reply", "message_id": 42, "text": "старый ответ"},
                {"type": "text", "text": "старый хвост"},
            ]
        }
    ).encode()
    client, http = make(body)
    try:
        stale = asyncio.run(client.catch_up(http))
        assert len(stale) == 2
        assert stale[0].type == "reply" and stale[0].message_id == 42
        assert client.since == 2  # the next poll starts past the stale part
    finally:
        asyncio.run(http.aclose())
