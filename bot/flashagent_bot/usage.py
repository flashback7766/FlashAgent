"""Token accounting per room per day, persisted so /usage and the budget
survive restarts. The serve's usage events carry prompt/completion split;
the room's name is the bucket."""

from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path


class UsageStore:
    def __init__(self, data_dir: Path):
        self.data_dir = data_dir

    def _file(self, day: str) -> Path:
        return self.data_dir / f"usage-{day}.json"

    def add(self, room: str, prompt: int, completion: int) -> None:
        if not prompt and not completion:
            return
        day = datetime.now(timezone.utc).strftime("%Y-%m-%d")
        f = self._file(day)
        try:
            data = json.loads(f.read_text(encoding="utf-8")) if f.exists() else {}
        except ValueError:
            data = {}
        cur = data.get(room, {"prompt": 0, "completion": 0})
        cur["prompt"] += prompt
        cur["completion"] += completion
        data[room] = cur
        self.data_dir.mkdir(parents=True, exist_ok=True)
        f.write_text(json.dumps(data, ensure_ascii=False), encoding="utf-8")

    def today(self) -> dict[str, dict[str, int]]:
        day = datetime.now(timezone.utc).strftime("%Y-%m-%d")
        f = self._file(day)
        try:
            return json.loads(f.read_text(encoding="utf-8")) if f.exists() else {}
        except ValueError:
            return {}

    def total(self, data: dict[str, dict[str, int]] | None = None) -> int:
        data = self.today() if data is None else data
        return sum(v.get("prompt", 0) + v.get("completion", 0) for v in data.values())
