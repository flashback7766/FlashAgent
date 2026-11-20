# FlashAgent Telegram bot

The Telegram face of FlashAgent: one bot process, many rooms. Every room in
the config is one deployment — its own backend (`flashagent --serve`), its
own session and memory vault, and its own mode:

- **The developer room** (`mode = "dev"`): the whole brain. The bot listens
  to the conversation, answers questions with the chat as context, drafts
  channel posts with publish/cancel buttons, writes facts into the memory
  vault, and surfaces approvals with Allow/Deny buttons.
- **The users' room** (`mode = "users"`): reports and questions on the
  compact tier. `/report` keeps a problem report (the bot stores it itself —
  no tokens burn on storing one); `/digest` asks the room's backend to group
  the batch and name the worst first. The room's backend runs `--prompt
  compact --mode manual`, so a report room can neither run a shell nor edit
  anything without the admins allowing it.

Every room's backend is bounded: the folder is read behind a two-second
clock, the prompt tier is set per room, and the room speaks only when
addressed — a reply to the bot, or a command.

## Setup

1. Backends: `flashagent --serve --dir <folder> --port <port>
   [--prompt compact] [--mode manual]` with `FLASHAGENT_SERVE_TOKEN=<secret>`
   in the environment. The config must already be set up (`flashagent
   --setup`); the model, the provider and the memory vault come from the same
   files the TUI uses.
2. Bot side: copy `config.example.toml` to `bot.toml`; fill in the BotFather
   token, the channel id, the admin ids, one `[[serve]]` block per backend,
   and one `[[chat]]` room per group naming its backend.
   `pip install -r requirements.txt`, then `python3 bot.py`.
3. Add the bot to the rooms (give it access to messages) and make it an admin
   in the channel it posts to.

## The commands, per room

Developer room: `/ask`, `/post`, `/remember`, `/status`, `/cancel`,
`/newsession`. Users' room: `/report`, `/digest` (admins), `/status`.

The commands are for the admins; the listening and the reply-to-ask are for
everyone in the room. Unanswered approvals are denied after the backend's
clock, and the model adapts.
