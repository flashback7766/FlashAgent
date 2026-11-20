# FlashAgent Telegram bot

The Telegram face of FlashAgent: a bot that sits in a group, listens to the
conversation, and talks to the FlashAgent backend (`flashagent --serve`) —
asks it questions with the chat as context, brings back answers, drafts
channel posts for approval, and keeps facts in the memory vault. The bot is
generic: one deployment per project group, pointed at one backend.

## Setup

1. FlashAgent side: `flashagent --serve --dir <project-folder> --port 8081`
   with `FLASHAGENT_SERVE_TOKEN=<secret>` in the environment. The config must
   already be set up (`flashagent --setup`); the model, tools, permissions and
   the memory vault come from the same files the TUI uses.
2. Bot side: copy `config.example.toml` to `bot.toml`, fill in the BotFather
   token, the group id, the channel id, the admin ids and the serve token.
   `pip install -r requirements.txt`, then `python3 bot.py`.
3. Add the bot to the group (give it access to messages) and make it an admin
   in the channel it posts to.

## What the bot does

- Listens to the group: the last messages ride along as context with every ask.
- `/ask <question>` — ask the backend something with the chat as context.
- Replying to one of its messages asks it the same way.
- `/post <topic>` — a channel post is drafted and shown with publish/cancel
  buttons; publishing goes to the configured channel.
- `/remember <fact>` — a fact goes into the memory vault.
- `/status`, `/cancel`, `/newsession` — the backend's live state, a running
  turn, a fresh session.
- An approval the backend waits for is posted with Allow/Deny buttons; only
  the admins answer. Unanswered approvals are denied after the backend's
  clock, and the model adapts.

The commands are for the admins; the listening and the reply-to-ask are for
everyone in the group.
