"""Entry point: python -m flashagent_bot <bot.toml>"""

from __future__ import annotations

import asyncio
import logging
import sys
from logging.handlers import RotatingFileHandler
from pathlib import Path

from .config import ConfigError, load
from .tgbot import App


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: python -m flashagent_bot <bot.toml>", file=sys.stderr)
        return 2
    try:
        cfg = load(Path(sys.argv[1]))
    except ConfigError as e:
        print(f"config: {e}", file=sys.stderr)
        return 2

    logging.basicConfig(
        level=logging.INFO, format="%(asctime)s %(name)s %(levelname)s %(message)s"
    )
    logs = cfg.data_dir / "logs"
    logs.mkdir(parents=True, exist_ok=True)
    fh = RotatingFileHandler(
        logs / "bot.log", maxBytes=5_000_000, backupCount=3, encoding="utf-8"
    )
    fh.setFormatter(logging.Formatter("%(asctime)s %(name)s %(levelname)s %(message)s"))
    logging.getLogger().addHandler(fh)

    try:
        asyncio.run(App(cfg).run())
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
