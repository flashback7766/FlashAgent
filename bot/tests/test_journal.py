"""The chat journal: JSON lines, a week of retention, torn lines dropped."""

import json
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

from flashagent_bot.journal import Journal


def test_append_and_tail(tmp_path):
    j = Journal(tmp_path / "room.jsonl")
    j.append("alice", "привет")
    j.append("bob", "привет, бот", kind="report")
    recs = j.tail(10)
    assert [r["who"] for r in recs] == ["alice", "bob"]
    assert recs[1]["kind"] == "report"
    assert recs[1]["text"] == "привет, бот"
    assert j.count == 2


def test_prune_drops_the_week_old_lines(tmp_path):
    p = tmp_path / "room.jsonl"
    old = datetime.now(timezone.utc) - timedelta(days=9)
    fresh = datetime.now(timezone.utc) - timedelta(days=1)
    p.write_text(
        json.dumps({"ts": old.isoformat(), "who": "a", "text": "old", "kind": "msg"})
        + "\n"
        + json.dumps({"ts": fresh.isoformat(), "who": "b", "text": "new", "kind": "msg"})
        + "\n",
        encoding="utf-8",
    )
    j = Journal(p)
    assert j.count == 1
    assert j.tail(10)[0]["text"] == "new"


def test_torn_lines_are_dropped(tmp_path):
    p = tmp_path / "room.jsonl"
    p.write_text(
        '{"ts": "2026-11-21T10:00:00+00:00", "who": "a", "text": "ok", "kind": "msg"}\n'
        '{"ts": "2026-11-21T10:',
        encoding="utf-8",
    )
    j = Journal(p)
    assert j.count == 1


def test_quiet_for_grows_after_append(tmp_path):
    j = Journal(tmp_path / "room.jsonl")
    assert j.quiet_for(time.monotonic()) > 1e8  # never written: effectively eternal
    j.append("a", "x")
    assert j.quiet_for(time.monotonic()) < 5.0
