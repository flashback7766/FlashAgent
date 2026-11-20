"""The room's chat log, written by the bot for zero tokens.

Every room message lands here as one JSON line; the brains read the file
with their own tools whenever they want the room's history. That is the
"smart steering" background: no buffering, no cost per message, the brain
decides itself when to look. A week of retention: old lines are dropped
on startup (and only then — the file is small and the brain may hold it).
"""

from __future__ import annotations

import json
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

RETENTION_DAYS = 7


class Journal:
    def __init__(self, path: Path):
        self.path = path
        self.count = 0  # lines kept; the pulse uses it as a watermark
        self.last_ts = 0.0  # monotonic; the pulse fires only after quiet
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._prune()

    def append(self, who: str, text: str, kind: str = "msg") -> None:
        line = json.dumps(
            {
                "ts": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                "who": who,
                "text": text,
                "kind": kind,
            },
            ensure_ascii=False,
        )
        with open(self.path, "a", encoding="utf-8") as f:
            f.write(line + "\n")
        self.count += 1
        self.last_ts = time.monotonic()

    def quiet_for(self, now: float) -> float:
        """Seconds since the last append; huge when nothing was ever written."""
        return now - self.last_ts if self.last_ts else 1e9

    def _prune(self) -> None:
        if not self.path.exists():
            return
        cutoff = datetime.now(timezone.utc) - timedelta(days=RETENTION_DAYS)
        kept: list[str] = []
        with open(self.path, encoding="utf-8") as f:
            for line in f:
                try:
                    rec = json.loads(line)
                    ts = datetime.fromisoformat(rec.get("ts", ""))
                    if ts >= cutoff:
                        kept.append(line.rstrip("\n"))
                except (ValueError, TypeError):
                    continue  # a torn line from a crash — drop it
        self.count = len(kept)
        tmp = self.path.with_suffix(".tmp")
        tmp.write_text("\n".join(kept) + ("\n" if kept else ""), encoding="utf-8")
        tmp.replace(self.path)

    def tail(self, n: int = 20) -> list[dict]:
        if not self.path.exists():
            return []
        out: list[dict] = []
        with open(self.path, encoding="utf-8") as f:
            for line in f.readlines()[-n:]:
                try:
                    out.append(json.loads(line))
                except ValueError:
                    continue
        return out
