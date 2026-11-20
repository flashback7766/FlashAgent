# FlashAgent Telegram bot (v2)

The Telegram face of FlashAgent: one bot process, one project, three room
kinds. The bot is transport only — the conversation lives in each room's
`flashagent --serve` session, the background chatter lives in journal
files the brains read themselves, and the state the bot owns (limits,
usage, mutes) sits in `data_dir`.

## The rooms

- **dev** — the team group. Full brain: commands, tools, approvals, the
  memory vault. The bot is an *active participant*: it journals every
  message, and after `pulse_minutes` of quiet it reads the journal and
  speaks up — or answers SKIP and stays silent. Addressed messages are
  `/ask`, a reply to the bot, or an @mention.
- **users** — the community chat. Free conversation with guardrails: the
  room's brain only reaches its own serve dir (reports, screenshots),
  requests are rate limited (`requests_per_hour`), and a spammer who
  pushes past the cooldown warning is ignored for `ignore_hours`.
  `/report` starts the issue pipeline: the bot discusses first, dedups
  against existing issues, then files one under the GitHub account.
- **channel** — broadcast only. Posts are drafted in the dev room with
  publish/redo/cancel buttons; series (the devlog) run on a schedule.

## The steering model

No buffer, no cost per background message:

- the bot writes **every** room message to the room's journal file
  (`journal`, relative to the serve's `dir`), zero tokens;
- a message that arrives **while the brain is already answering** flies
  into the running turn via serve's `/steer` instead of queueing;
- the users' room runs with a bounded context (`--context-cap`,
  auto-compaction at `--compact-at`), so a long community day cannot
  blow the window.

## Setup

1. Brains: `flashagent --serve --dir <folder> --port <port>
   [--prompt compact] [--mode manual] [--context-cap N] [--compact-at N]`
   with `FLASHAGENT_SERVE_TOKEN=<secret>` in the environment. The model,
   the provider and the vault come from the same files the TUI uses.
2. Bot: copy `config.example.toml` to `bot.toml`; fill in the BotFather
   token, the admin ids, one `[serve.<name>]` per brain, one
   `[chat.<name>]` per room. Then `pip install -r requirements.txt` and
   `python3 -m flashagent_bot bot.toml`.
3. Add the bot to the rooms (message access on) and make it an admin in
   the channel it posts to.

4. systemd --user units live in `deploy/`. Install them and switch on
   linger — without linger nothing comes back after a reboot:
   ```
   cp deploy/*.service ~/.config/systemd/user/ && systemctl --user daemon-reload
   systemctl --user enable --now flashagent-serve-dev flashagent-serve-users flashagent-bot
   loginctl enable-linger $USER
   ```

## Commands

`/ask`, `/report` — everyone. `/post`, `/remember`, `/digest`, `/usage`,
`/status`, `/cancel`, `/newsession` — admins. Approvals surface as
Allow/Deny buttons; unanswered ones are denied by the serve's clock.

## Tests

`pytest tests/` — config validation, the journal retention, the rate
limiter (including the mute surviving a restart), token accounting and
the Telegram chunking. The suite runs in CI with the Rust gates.
