"""Config loading: one TOML file describes the whole installation.

A room is dev / users / channel. dev and users each name a [serve.<name>]
brain; the channel has no brain of its own — posts to it are drafted in
the dev room. Everything the operator must fill in is validated here, so
the bot dies loudly at startup instead of quietly misbehaving at runtime.
"""

from __future__ import annotations

import tomllib
from dataclasses import dataclass
from pathlib import Path


class ConfigError(Exception):
    """The config is unusable; the message tells the operator what to fix."""


@dataclass
class ServeBrain:
    name: str
    url: str
    token: str
    dir: str  # the serve project dir; the bot drops chat journals here


@dataclass
class Room:
    name: str
    chat_id: int
    kind: str  # dev | users | channel
    serve: ServeBrain | None  # channel rooms have none
    journal: str | None  # relative to the serve dir; the brain reads it
    pulse_minutes: int | None = None  # dev: speak only after this much quiet
    morning_digest: str | None = None  # users: "HH:MM", posted to the dev room
    devlog: str | None = None  # channel: "Weekday HH:MM" series schedule


@dataclass
class Github:
    token: str
    repo: str  # "owner/name"


@dataclass
class Config:
    token: str
    admins: list[int]
    data_dir: Path
    brains: dict[str, ServeBrain]
    rooms: list[Room]
    github: Github | None
    requests_per_hour: int
    ignore_hours: int
    daily_tokens: int  # soft cap per day; 0 = unlimited


def load(path: Path) -> Config:
    if not path.exists():
        raise ConfigError(
            f"{path} is missing: copy config.example.toml to bot.toml and fill it in"
        )
    with open(path, "rb") as f:
        raw = tomllib.load(f)

    bot = raw.get("bot") or {}
    token = bot.get("token")
    if not token:
        raise ConfigError("[bot].token is empty: the BotFather token goes there")
    admins = [int(a) for a in (bot.get("admins") or [])]
    if not admins:
        raise ConfigError("[bot].admins is empty: at least one user id goes there")
    data_dir = Path(bot.get("data_dir") or "./data").expanduser()

    brains: dict[str, ServeBrain] = {}
    for name, s in (raw.get("serve") or {}).items():
        if not s.get("url"):
            raise ConfigError(f"[serve.{name}].url is empty")
        brains[name] = ServeBrain(
            name=name,
            url=str(s["url"]).rstrip("/"),
            token=s.get("token", ""),
            dir=str(s.get("dir", "")).rstrip("/"),
        )

    rooms: list[Room] = []
    seen_ids: set[int] = set()
    for name, c in (raw.get("chat") or {}).items():
        kind = c.get("kind")
        if kind not in ("dev", "users", "channel"):
            raise ConfigError(
                f"[chat.{name}].kind must be dev, users or channel, got {kind!r}"
            )
        chat_id = c.get("chat_id")
        if not isinstance(chat_id, int):
            raise ConfigError(
                f"[chat.{name}].chat_id must be the room id from @userinfobot"
                " (negative for groups)"
            )
        if chat_id in seen_ids:
            raise ConfigError(f"[chat.{name}] reuses chat_id {chat_id}")
        seen_ids.add(chat_id)

        serve = None
        if kind != "channel":
            serve = brains.get(str(c.get("serve", "")))
            if serve is None:
                raise ConfigError(
                    f"[chat.{name}].serve points nowhere: no [serve.{c.get('serve')}]"
                )

        journal = c.get("journal")
        if journal and (serve is None or not serve.dir):
            raise ConfigError(
                f"[chat.{name}] has a journal but its serve has no dir to put it in"
            )
        pulse = c.get("pulse_minutes")
        if pulse is not None and kind != "dev":
            raise ConfigError(f"[chat.{name}].pulse_minutes only makes sense for dev")
        digest = c.get("morning_digest")
        if digest is not None and kind != "users":
            raise ConfigError(
                f"[chat.{name}].morning_digest only makes sense for users"
            )
        _check_time(f"[chat.{name}].morning_digest", digest)
        devlog = c.get("devlog")
        if devlog is not None and kind != "channel":
            raise ConfigError(f"[chat.{name}].devlog only makes sense for channel")

        rooms.append(
            Room(
                name=name,
                chat_id=chat_id,
                kind=kind,
                serve=serve,
                journal=journal,
                pulse_minutes=pulse,
                morning_digest=digest,
                devlog=devlog,
            )
        )
    if not rooms:
        raise ConfigError("no [chat.<name>] rooms: every room names its backend")

    gh = raw.get("github") or {}
    github = None
    if gh:
        if not gh.get("token") or not gh.get("repo"):
            raise ConfigError("[github] needs both token and repo")
        github = Github(token=gh["token"], repo=gh["repo"])

    limits = raw.get("limits") or {}
    budget = raw.get("budget") or {}
    return Config(
        token=str(token),
        admins=admins,
        data_dir=data_dir,
        brains=brains,
        rooms=rooms,
        github=github,
        requests_per_hour=int(limits.get("requests_per_hour", 30)),
        ignore_hours=int(limits.get("ignore_hours", 12)),
        daily_tokens=int(budget.get("daily_tokens", 0)),
    )


def _check_time(where: str, value: object) -> None:
    if value is None:
        return
    parts = str(value).split(":")
    if len(parts) != 2:
        raise ConfigError(f"{where} must be HH:MM (24h), got {value!r}")
    try:
        hh, mm = int(parts[0]), int(parts[1])
    except ValueError:
        raise ConfigError(f"{where} must be HH:MM (24h), got {value!r}") from None
    if not (0 <= hh <= 23 and 0 <= mm <= 59):
        raise ConfigError(f"{where} must be HH:MM (24h), got {value!r}")
