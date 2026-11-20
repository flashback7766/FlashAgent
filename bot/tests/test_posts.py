"""The series schedule: `Mon 18:00` and friends must parse or must not."""

from flashagent_bot.posts import parse_series


def test_weekday_and_time():
    assert parse_series("Mon 18:00") == (0, (18, 0))
    assert parse_series("sun 09:30") == (6, (9, 30))


def test_unreadable_specs_are_none():
    assert parse_series("Monday evening") is None
    assert parse_series("Mon 25:00") is None
    assert parse_series("Fri 9") is None
    assert parse_series("") is None


def test_prefix_day_names_work():
    # `Monday 18:00` names the same day as `Mon 18:00`
    assert parse_series("Monday 18:00") == (0, (18, 0))
