"""The rate limiter: N/hour, a warning, then a 12-hour mute for spammers.
The mute must survive a restart (state file), so tests reload a limiter
from the same state path to prove it."""

import json

from flashagent_bot.limits import ALLOW, COOLDOWN, MUTED, RateLimiter

HOUR = 3600.0


def make(tmp_path, per_hour=3, ignore_hours=12, warn_burst=3):
    return RateLimiter(
        per_hour, ignore_hours, tmp_path / "limits.json", warn_burst=warn_burst
    )


def test_allows_up_to_the_limit(tmp_path):
    rl = make(tmp_path, per_hour=3)
    now = 1000.0
    for i in range(3):
        assert rl.check(7, now=now + i)[0] == ALLOW
    assert rl.check(7, now=now + 3)[0] == COOLDOWN


def test_window_slides(tmp_path):
    rl = make(tmp_path, per_hour=2)
    now = 1000.0
    assert rl.check(7, now=now)[0] == ALLOW
    assert rl.check(7, now=now + 10)[0] == ALLOW
    verdict, secs = rl.check(7, now=now + 20)
    assert verdict == COOLDOWN
    # an hour after the first request the slot frees up again
    assert rl.check(7, now=now + HOUR + 30)[0] == ALLOW


def test_spam_after_warning_turns_into_a_mute(tmp_path):
    rl = make(tmp_path, per_hour=1, ignore_hours=12, warn_burst=3)
    now = 1000.0
    assert rl.check(7, now=now)[0] == ALLOW
    assert rl.check(7, now=now + 10)[0] == COOLDOWN
    assert rl.check(7, now=now + 20)[0] == COOLDOWN
    verdict, secs = rl.check(7, now=now + 30)
    assert verdict == MUTED
    assert abs(secs - 12 * HOUR) < 5.0


def test_mute_survives_a_restart(tmp_path):
    rl = make(tmp_path, per_hour=1)
    now = 1000.0
    rl.check(7, now=now)
    for i in range(1, 4):
        rl.check(7, now=now + i * 10)
    verdict, _ = rl.check(7, now=now + 40)
    assert verdict == MUTED
    # a fresh limiter over the same state file must remember the mute
    rl2 = make(tmp_path, per_hour=1)
    verdict, _ = rl2.check(7, now=now + 50)
    assert verdict == MUTED


def test_mute_expires(tmp_path):
    rl = make(tmp_path, per_hour=1, ignore_hours=2)
    now = 1000.0
    rl.check(7, now=now)
    for i in range(1, 4):
        rl.check(7, now=now + i * 10)
    verdict, _ = rl.check(7, now=now + 40)
    assert verdict == MUTED
    assert rl.check(7, now=now + 2 * HOUR + 60)[0] == ALLOW


def test_state_file_is_json(tmp_path):
    rl = make(tmp_path)
    rl.check(7, now=1000.0)
    raw = json.loads((tmp_path / "limits.json").read_text(encoding="utf-8"))
    assert "7" in raw["windows"]  # JSON keys are strings by definition


def test_per_hour_override_tightens(tmp_path):
    rl = make(tmp_path, per_hour=30)
    now = 1000.0
    for i in range(5):
        assert rl.check(7, now=now + i, per_hour=5)[0] == ALLOW
    assert rl.check(7, now=now + 5, per_hour=5)[0] == COOLDOWN
