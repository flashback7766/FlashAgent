"""Per-user request limits for the users' room: 30/hour, and a spammer who
keeps pushing after the cooldown warning gets ignored for 12 hours.

State survives restarts: a mute must not evaporate because the bot
crashed. The whole store is one small JSON file rewritten on change.
"""

from __future__ import annotations

import json
import time
from pathlib import Path

ALLOW = "allow"
COOLDOWN = "cooldown"
MUTED = "muted"


class RateLimiter:
    def __init__(
        self,
        per_hour: int,
        ignore_hours: int,
        state_path: Path,
        warn_burst: int = 3,
    ):
        self.per_hour = per_hour
        self.ignore_hours = ignore_hours
        self.warn_burst = warn_burst  # pushes past the warning before the mute
        self.state_path = state_path
        self.windows: dict[int, list[float]] = {}
        self.warned: dict[int, list[float]] = {}
        self.mutes: dict[int, float] = {}
        self._load()

    def check(
        self,
        user_id: int,
        now: float | None = None,
        per_hour: int | None = None,
    ) -> tuple[str, float]:
        """Verdict for one request: (verdict, seconds). allow → 0.0,
        cooldown → retry after that many seconds, muted → time remaining.
        `per_hour` overrides the configured limit — economy mode tightens
        the same machinery instead of inventing a second one."""
        now = time.time() if now is None else now
        limit = per_hour or self.per_hour

        until = self.mutes.get(user_id, 0.0)
        if now < until:
            return (MUTED, until - now)
        self.mutes.pop(user_id, None)

        hour_ago = now - 3600.0
        window = [t for t in self.windows.get(user_id, []) if t > hour_ago]
        if len(window) < limit:
            window.append(now)
            self.windows[user_id] = window
            self._save()
            return (ALLOW, 0.0)

        # Over the limit. A few more pushes after the warning and the
        # silence becomes a real mute.
        pushes = [t for t in self.warned.get(user_id, []) if t > hour_ago]
        if len(pushes) + 1 >= self.warn_burst:
            mute_until = now + self.ignore_hours * 3600.0
            self.mutes[user_id] = mute_until
            self.warned.pop(user_id, None)
            self.windows[user_id] = []
            self._save()
            return (MUTED, mute_until - now)
        pushes.append(now)
        self.warned[user_id] = pushes
        self._save()
        retry = window[0] + 3600.0 - now
        return (COOLDOWN, max(retry, 60.0))

    def _load(self) -> None:
        if not self.state_path.exists():
            return
        try:
            raw = json.loads(self.state_path.read_text(encoding="utf-8"))
            self.windows = {int(k): v for k, v in raw.get("windows", {}).items()}
            self.warned = {int(k): v for k, v in raw.get("warned", {}).items()}
            self.mutes = {int(k): v for k, v in raw.get("mutes", {}).items()}
        except (ValueError, AttributeError):
            pass  # a torn file — start clean, limits are not precious data

    def _save(self) -> None:
        self.state_path.parent.mkdir(parents=True, exist_ok=True)
        self.state_path.write_text(
            json.dumps(
                {"windows": self.windows, "warned": self.warned, "mutes": self.mutes},
                ensure_ascii=False,
            ),
            encoding="utf-8",
        )
