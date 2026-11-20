"""The channel pipeline: a draft in the dev room with publish/redo/cancel
buttons, and the series (the weekly devlog) on a schedule. The brain
writes the post as HTML; Telegram rejects malformed markup, so every
publish falls back to plain text instead of eating the post."""

from __future__ import annotations

WEEKDAYS = {"mon": 0, "tue": 1, "wed": 2, "thu": 3, "fri": 4, "sat": 5, "sun": 6}


def parse_series(spec: str) -> tuple[int, tuple[int, int]] | None:
    """`Mon 18:00` → (0, (18, 0)). None when it names nothing readable —
    the loop logs it once and skips the room rather than guessing."""
    parts = spec.strip().split()
    if len(parts) != 2:
        return None
    day = WEEKDAYS.get(parts[0][:3].lower())
    if day is None:
        return None
    try:
        hh, mm = (int(p) for p in parts[1].split(":"))
    except ValueError:
        return None
    if not (0 <= hh <= 23 and 0 <= mm <= 59):
        return None
    return (day, (hh, mm))
