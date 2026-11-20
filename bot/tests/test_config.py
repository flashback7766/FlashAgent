"""Config validation: the bot dies loudly at startup, so every rejection
the loader promises must actually happen."""

import pytest

from flashagent_bot.config import ConfigError, load

VALID = """
[bot]
token = "t"
admins = [1, 2]
data_dir = "./data"

[serve.dev]
url = "http://127.0.0.1:8093"
token = "k"
dir = "/tmp/brain"

[chat.dev]
chat_id = -1001
kind = "dev"
serve = "dev"
journal = "chat/dev-room.jsonl"
pulse_minutes = 12
"""


def write(tmp_path, text: str):
    p = tmp_path / "bot.toml"
    p.write_text(text, encoding="utf-8")
    return p


def test_valid_config_parses(tmp_path):
    cfg = load(write(tmp_path, VALID))
    assert cfg.token == "t"
    assert cfg.admins == [1, 2]
    assert len(cfg.rooms) == 1
    room = cfg.rooms[0]
    assert room.kind == "dev"
    assert room.serve is not None and room.serve.dir == "/tmp/brain"
    assert room.pulse_minutes == 12


def test_missing_file_names_the_example(tmp_path):
    with pytest.raises(ConfigError, match="config.example.toml"):
        load(tmp_path / "nope.toml")


def test_empty_token_rejected(tmp_path):
    with pytest.raises(ConfigError, match="token"):
        load(write(tmp_path, VALID.replace('token = "t"', 'token = ""')))


def test_no_admins_rejected(tmp_path):
    with pytest.raises(ConfigError, match="admins"):
        load(write(tmp_path, VALID.replace("admins = [1, 2]", "admins = []")))


def test_unknown_kind_rejected(tmp_path):
    with pytest.raises(ConfigError, match="kind"):
        load(write(tmp_path, VALID.replace('kind = "dev"', 'kind = "gang"')))


def test_users_room_needs_its_serve(tmp_path):
    text = VALID.replace('kind = "dev"', 'kind = "users"').replace(
        "serve = \"dev\"\njournal", "serve = \"users\"\njournal"
    )
    with pytest.raises(ConfigError, match="serve"):
        load(write(tmp_path, text))


def test_duplicate_chat_id_rejected(tmp_path):
    dup = VALID + '\n[chat.users]\nchat_id = -1001\nkind = "users"\nserve = "dev"\n'
    with pytest.raises(ConfigError, match="reuses chat_id"):
        load(write(tmp_path, dup))


def test_pulse_on_users_room_rejected(tmp_path):
    text = VALID.replace('kind = "dev"', 'kind = "users"')
    with pytest.raises(ConfigError, match="pulse_minutes"):
        load(write(tmp_path, text))


def test_bad_digest_time_rejected(tmp_path):
    text = VALID.replace(
        'pulse_minutes = 12', 'pulse_minutes = 12\nmorning_digest = "25:00"'
    )
    with pytest.raises(ConfigError, match="morning_digest"):
        load(write(tmp_path, text))


def test_journal_without_serve_dir_rejected(tmp_path):
    text = VALID.replace('dir = "/tmp/brain"', 'dir = ""')
    with pytest.raises(ConfigError, match="no dir"):
        load(write(tmp_path, text))
