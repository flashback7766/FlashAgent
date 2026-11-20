"""The report pipeline, without a live GitHub or a live brain.

The sentinel parsing is the contract between the users' brain (which
decides when a report is clear) and the bot (which does the filing);
the dedup decision parsing is the contract with the dev brain. The
client is exercised against a mock transport, so the request shapes
are pinned without a repo to talk to."""

import asyncio
import json

import httpx
import pytest

from flashagent_bot.issues import GithubClient, GithubError
from flashagent_bot.util import parse_dedup_decision, parse_issue_sentinel, search_keywords


def test_the_sentinel_is_an_order_not_a_message():
    text = "Понял проблему.\nСкорее всего баг в плеере.\nISSUE: Плейлист сбрасывается при повороте"
    clean, title = parse_issue_sentinel(text)
    assert title == "Плейлист сбрасывается при повороте"
    assert clean == "Понял проблему.\nСкорее всего баг в плеере."


def test_no_sentinel_leaves_the_text_whole():
    text = "Уточни, на каком устройстве?"
    clean, title = parse_issue_sentinel(text)
    assert title is None
    assert clean == text


def test_empty_title_is_no_title():
    clean, title = parse_issue_sentinel("готово\nISSUE:")
    assert title is None


def test_dedup_verdicts():
    assert parse_dedup_decision("REPLY: 12") == 12
    assert parse_dedup_decision("reply: 7") == 7  # the brain may not shout
    assert parse_dedup_decision("NEW") is None
    assert parse_dedup_decision("") is None
    assert parse_dedup_decision("REPLY:") is None  # a number or a new issue


def test_search_keywords_are_clean_words():
    assert search_keywords("Плейлист сбрасывается при повороте экрана!!!") == (
        "Плейлист сбрасывается при повороте экрана"
    )
    assert "и" not in search_keywords("не работает и всё").split()


def make_client(handler):
    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    return GithubClient("tok", "rentalx-wifi/rx-playa"), http


def test_create_posts_title_and_body():
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        seen["body"] = json.loads(request.content)
        return httpx.Response(201, json={"number": 7, "html_url": "https://x/i/7"})

    client, http = make_client(handler)
    try:
        issue = asyncio.run(client.create(http, "Заголовок", "Тело"))
    finally:
        asyncio.run(http.aclose())
    assert issue == {"number": 7, "url": "https://x/i/7"}
    assert seen["url"].endswith("/repos/rentalx-wifi/rx-playa/issues")
    assert seen["body"] == {"title": "Заголовок", "body": "Тело"}


def test_search_scopes_the_query_to_open_issues():
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["q"] = request.url.params["q"]
        return httpx.Response(200, json={"items": [{"number": 3, "title": "t", "html_url": "u"}]})

    client, http = make_client(handler)
    try:
        items = asyncio.run(client.search_open(http, "плейлист поворот"))
    finally:
        asyncio.run(http.aclose())
    assert seen["q"] == "repo:rentalx-wifi/rx-playa is:issue is:open плейлист поворот"
    assert items[0]["number"] == 3


def test_a_failure_raises_what_the_room_sees():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(422, json={"message": "bad"})

    client, http = make_client(handler)
    try:
        with pytest.raises(GithubError):
            asyncio.run(client.create(http, "x", "y"))
    finally:
        asyncio.run(http.aclose())


def test_closed_by_author_since_scopes_the_query():
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["q"] = request.url.params["q"]
        return httpx.Response(200, json={"items": [{"number": 9, "title": "t", "html_url": "u", "closed_at": "2026-11-21T10:00:00Z"}]})

    client, http = make_client(handler)
    try:
        items = asyncio.run(
            client.closed_by_author_since(http, "FlashAgent-bot", "2026-11-21T00:00:00Z")
        )
    finally:
        asyncio.run(http.aclose())
    assert seen["q"] == (
        "repo:rentalx-wifi/rx-playa is:issue is:closed"
        " author:FlashAgent-bot closed:>=2026-11-21T00:00:00Z"
    )
    assert items[0]["number"] == 9
    assert items[0]["closed_at"] == "2026-11-21T10:00:00Z"


def test_whoami_returns_the_login():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={"login": "FlashAgent-bot"})

    client, http = make_client(handler)
    try:
        login = asyncio.run(client.whoami(http))
    finally:
        asyncio.run(http.aclose())
    assert login == "FlashAgent-bot"
