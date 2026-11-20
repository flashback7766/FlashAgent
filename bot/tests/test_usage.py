"""Token accounting: per room per day, summed for the budget check."""

from flashagent_bot.usage import UsageStore


def test_add_accumulates_per_room(tmp_path):
    u = UsageStore(tmp_path)
    u.add("dev", 100, 50)
    u.add("dev", 10, 5)
    u.add("users", 200, 30)
    today = u.today()
    assert today["dev"] == {"prompt": 110, "completion": 55}
    assert today["users"] == {"prompt": 200, "completion": 30}
    assert u.total(today) == 395


def test_zero_usage_writes_nothing(tmp_path):
    u = UsageStore(tmp_path)
    u.add("dev", 0, 0)
    assert u.today() == {}
    assert u.total() == 0


def test_torn_file_starts_clean(tmp_path):
    (tmp_path / "usage-2099-01-01.json").write_text("{broken", encoding="utf-8")
    u = UsageStore(tmp_path)
    assert u.today() == {}  # a different day anyway; the point is no crash
