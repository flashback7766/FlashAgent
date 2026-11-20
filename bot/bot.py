"""FlashAgent, the Telegram face.

One bot process, many rooms. Every room in the config is one deployment: its
own backend (`flashagent --serve`), its own session and vault, and its own
mode. The developer room gets the whole brain — plans, tools, approvals,
channel posts. The users' room collects reports and answers questions on the
compact tier, and speaks only when addressed: nobody's tokens burn on
silence. The brain does the work; this process only carries messages and
buttons.
"""

import asyncio
import json
import logging
import pathlib
import time
from collections import deque
from dataclasses import dataclass

import tomllib
from aiogram import Bot, Dispatcher, F
from aiogram.client.default import DefaultBotProperties
from aiogram.enums import ParseMode
from aiogram.filters import Command, CommandObject
from aiogram.types import (
    CallbackQuery,
    InlineKeyboardButton,
    InlineKeyboardMarkup,
    Message,
)

log = logging.getLogger("flashagent-bot")
HERE = pathlib.Path(__file__).parent


@dataclass
class ServeEvent:
    """One entry of the backend's event feed, already flattened."""

    type: str
    text: str = ""
    name: str = ""
    reason: str = ""


class ConfigError(Exception):
    pass


def load_config(path: pathlib.Path) -> dict:
    if not path.exists():
        raise ConfigError(
            f"config {path} is missing: copy config.example.toml to bot.toml and fill it in"
        )
    with open(path, "rb") as f:
        cfg = tomllib.load(f)
    if not cfg.get("telegram", {}).get("token"):
        raise ConfigError("config [telegram].token is empty: the BotFather token goes there")
    if not cfg.get("chat"):
        raise ConfigError("config has no [chat.<name>] rooms: every room names its backend")
    return cfg


def escape(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def split_reply(text: str, limit: int = 3800) -> list[str]:
    if len(text) <= limit:
        return [text]
    parts = []
    while text:
        parts.append(text[:limit])
        text = text[limit:]
    return parts


class Brain:
    """The HTTP client for one `flashagent --serve`. Polls, does not stream: a
    turn is minutes, a poll a second."""

    def __init__(self, url: str, token: str):
        self.url = url.rstrip("/")
        self.token = token
        self.since = 0

    def _headers(self) -> dict:
        return {"Authorization": f"Bearer {self.token}"} if self.token else {}

    async def status(self, session) -> dict:
        async with session.get(f"{self.url}/status", headers=self._headers()) as r:
            return await r.json()

    async def start_turn(self, session, text: str) -> tuple[bool, str]:
        async with session.post(
            f"{self.url}/turn", headers=self._headers(), json={"text": text}
        ) as r:
            body = await r.json()
            return (r.status == 200), body.get("error", f"HTTP {r.status}")

    async def approve(self, session, allow: bool) -> None:
        async with session.post(
            f"{self.url}/approve", headers=self._headers(), json={"allow": allow}
        ) as r:
            await r.read()

    async def cancel(self, session) -> None:
        async with session.post(f"{self.url}/cancel", headers=self._headers()) as r:
            await r.read()

    async def reset(self, session) -> None:
        async with session.post(f"{self.url}/reset", headers=self._headers()) as r:
            await r.read()

    async def poll(self, session) -> list[ServeEvent]:
        """New events since the last poll, oldest first."""
        async with session.get(
            f"{self.url}/events", params={"since": self.since}, headers=self._headers()
        ) as r:
            body = await r.json()
        events = body.get("events", [])
        self.since += len(events)
        out = []
        for ev in events:
            kind = ev.get("type", "")
            if kind == "text":
                out.append(ServeEvent("text", text=ev.get("text", "")))
            elif kind == "tool_start":
                out.append(ServeEvent("tool", name=ev.get("name", "")))
            elif kind == "done":
                out.append(ServeEvent("done", reason=ev.get("reason", "")))
            elif kind == "turn_failed":
                out.append(ServeEvent("failed", text=body.get("last_error") or "unknown error"))
            elif kind == "approval":
                args = ev.get("args_json")
                if isinstance(args, dict):
                    args = json.dumps(args, ensure_ascii=False)
                out.append(ServeEvent("approval", name=f"{ev.get('tool', '')} {args or ''}"))
            elif kind == "approval_timeout":
                out.append(ServeEvent("approval_timeout", name=ev.get("tool", "")))
        return out


class Room:
    """One chat of the config: its backend, its context, its rules. The dev
    room gets the whole brain; the users' room collects reports on the
    compact tier and answers only when addressed."""

    def __init__(self, name: str, room_cfg: dict, serve: dict):
        self.name = name
        self.mode = room_cfg.get("mode", "dev")
        self.chat_id = int(room_cfg["chat_id"])
        self.brain = Brain(serve["url"], serve.get("token", ""))
        self.poll_interval = float(room_cfg.get("poll_interval", 1.0))
        self.collector = ChatCollector(int(room_cfg.get("context_messages", 20)))
        self.reports_path = pathlib.Path(room_cfg.get("reports_file", ""))
        self.pending_post: dict[int, str] = {}

    def is_admin(self, cfg: dict, user_id: int | None) -> bool:
        return user_id in set(cfg["telegram"].get("admin_ids", []))


class ChatCollector:
    """The last messages of the room, the context every ask rides on."""

    def __init__(self, limit: int):
        self.limit = limit
        self.lines: deque[str] = deque(maxlen=limit)

    def add(self, who: str, text: str) -> None:
        text = (text or "").strip()
        if not text or text.startswith("/"):
            return
        self.lines.append(f"{who}: {text}")

    def context(self) -> str:
        if not self.lines:
            return "(the room has been quiet)"
        return "\n".join(self.lines)


def escape(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def split_reply(text: str, limit: int = 3800) -> list[str]:
    if len(text) <= limit:
        return [text]
    parts = []
    while text:
        parts.append(text[:limit])
        text = text[limit:]
    return parts


class BotApp:
    """The bot over all rooms: one Telegram bot, per-room backends."""

    def __init__(self, cfg: dict):
        tg = cfg["telegram"]
        self.cfg = cfg
        self.bot = Bot(tg["token"], default=DefaultBotProperties(parse_mode=ParseMode.HTML))
        self.channel_id = int(tg["channel_id"])
        self.rooms: dict[int, Room] = {}
        for name, room_cfg in cfg.get("chat", {}).items():
            serve_name = room_cfg.get("serve", name)
            serve = cfg.get("serve", {}).get(serve_name)
            if not serve:
                raise ConfigError(f"room {name} names backend {serve_name!r} which is not in [serve]")
            self.rooms[int(room_cfg["chat_id"])] = Room(name, room_cfg, serve)
        self.pending_post: dict[int, tuple[int, str]] = {}

    def room_by_chat(self, chat_id: int) -> Room | None:
        return self.rooms.get(int(chat_id))

    def is_admin(self, chat_id: int, user_id: int | None) -> bool:
        return user_id in set(self.cfg["telegram"].get("admin_ids", []))

    async def run_turn(self, message: Message, room: Room, prompt: str) -> None:
        """One turn against the room's backend. The live line is edited as
        tools run; the finished answer is sent in chunks."""
        note = await message.answer("💭 думаю…")
        ok, why = await room.brain.start_turn(self.bot.session, prompt)
        if not ok:
            await note.edit_text(f"⚠️ {why}")
            return
        answer = []
        last_edit = 0.0
        failed = None
        while True:
            await asyncio.sleep(room.poll_interval)
            events = await room.brain.poll(self.bot.session)
            for ev in events:
                if ev.type == "text":
                    answer.append(ev.text)
                elif ev.type == "tool":
                    now = time.monotonic()
                    if now - last_edit > 2.0:
                        last_edit = now
                        try:
                            await note.edit_text(f"🔧 {ev.name}")
                        except Exception:
                            pass
                elif ev.type == "approval":
                    await self.ask_approval(message, ev.name)
                elif ev.type == "failed":
                    failed = ev.text
            if failed is not None:
                await note.edit_text(f"⚠️ the turn failed: {failed}")
                return
            if any(ev.type == "done" for ev in events):
                break
        text = "".join(answer).strip() or "(empty answer)"
        await note.delete()
        for part in split_reply(escape(text)):
            await message.answer(part)

    async def ask_approval(self, message: Message, what: str) -> None:
        keyboard = InlineKeyboardMarkup(
            inline_keyboard=[
                [
                    InlineKeyboardButton(text="✅ Allow", callback_data="approve:1"),
                    InlineKeyboardButton(text="❌ Deny", callback_data="approve:0"),
                ]
            ]
        )
        await message.answer(f"🔒 the backend asks to run <b>{escape(what)}</b>", reply_markup=keyboard)

    async def handle_approve(self, query: CallbackQuery) -> None:
        if not self.is_admin(query.message.chat.id, query.from_user.id):
            await query.answer("only the admins answer this", show_alert=True)
            return
        room = self.room_by_chat(query.message.chat.id)
        if not room:
            return
        allow = query.data.endswith("1")
        await room.brain.approve(self.bot.session, allow)
        await query.answer("allowed" if allow else "denied")

    async def cmd_ask(self, message: Message, room: Room, question: str) -> None:
        if not question:
            await message.answer("usage: /ask <question>")
            return
        prompt = (
            "You are FlashAgent, sitting in a group chat. The recent chat:\n\n"
            f"{room.collector.context()}\n\n"
            f"A member asked: {question}\nAnswer them directly."
        )
        await self.run_turn(message, room, prompt)

    async def cmd_post(self, message: Message, room: Room, topic: str) -> None:
        if not topic:
            await message.answer("usage: /post <what the post is about>")
            return
        prompt = (
            "Draft a Telegram channel post. The recent group chat:\n\n"
            f"{room.collector.context()}\n\n"
            f"The post is about: {topic}\n"
            "Write the post itself, ready to publish, in the chat's language. No preamble."
        )
        note = await message.answer("📝 черновик пишется…")
        ok, why = await room.brain.start_turn(self.bot.session, prompt)
        if not ok:
            await note.edit_text(f"⚠️ {why}")
            return
        answer = []
        failed = None
        while True:
            await asyncio.sleep(room.poll_interval)
            events = await room.brain.poll(self.bot.session)
            for ev in events:
                if ev.type == "text":
                    answer.append(ev.text)
                elif ev.type == "approval":
                    await self.ask_approval(message, ev.name)
                elif ev.type == "failed":
                    failed = ev.text
            if failed is not None or any(ev.type == "done" for ev in events):
                break
        draft = "".join(answer).strip()
        await note.delete()
        if failed is not None:
            await message.answer(f"⚠️ the turn failed: {failed}")
            return
        if not draft:
            await message.answer("⚠️ the backend returned nothing")
            return
        keyboard = InlineKeyboardMarkup(
            inline_keyboard=[
                [
                    InlineKeyboardButton(text="✅ Опубликовать", callback_data="post_yes"),
                    InlineKeyboardButton(text="🗑 Отмена", callback_data="post_cancel"),
                ]
            ]
        )
        sent = await message.answer(f"<b>Черновик поста:</b>\n\n{escape(draft)}", reply_markup=keyboard)
        self.pending_post[sent.message_id] = (self.channel_id, draft)

    async def cmd_remember(self, message: Message, room: Room, fact: str) -> None:
        if not fact:
            await message.answer("usage: /remember <what to keep>")
            return
        await self.run_turn(
            message,
            room,
            f"Remember this fact in the memory vault, exactly as it was said, under a fitting name: {fact}",
        )

    async def cmd_report(self, message: Message, room: Room, report: str) -> None:
        """A user's problem report: the bot keeps it itself — no tokens burn
        on storing a report — and the room's backend only turns a batch into
        a digest when the admins ask for one."""
        if not report:
            await message.answer("usage: /report <what went wrong>")
            return
        who = message.from_user.full_name if message.from_user else "?"
        stamp = time.strftime("%Y-%m-%d %H:%M")
        if room.reports_path and str(room.reports_path):
            room.reports_path.parent.mkdir(parents=True, exist_ok=True)
            with open(room.reports_path, "a", encoding="utf-8") as f:
                f.write(f"\n## {stamp} — {who}\n{report}\n")
            await message.answer(
                "✅ the report is kept — thank you. The developers read these; "
                "a GitHub issue is still the fastest way to get a fix."
            )
        else:
            await message.answer("✅ the report is noted (the room keeps no report file — tell the developers)")

    async def cmd_digest(self, message: Message, room: Room) -> None:
        """The admins' look at the reports: the backend reads the report file
        and answers with what it saw. One turn, on the room's tier."""
        if not room.reports_path or not room.reports_path.exists():
            await message.answer("no reports yet")
            return
        content = room.reports_path.read_text(encoding="utf-8")[-16_000:]
        prompt = (
            "These are the problem reports users of the project sent without GitHub. "
            "Group them by what broke, count the repeats, name the worst first, and "
            "say in one line what a fixer should look at first. Be terse.\n\n" + content
        )
        await self.run_turn(message, room, prompt)

    async def handle_post_callback(self, query: CallbackQuery) -> None:
        if not self.is_admin(query.message.chat.id, query.from_user.id):
            await query.answer("only the admins publish", show_alert=True)
            return
        action = query.data
        entry = self.pending_post.pop(query.message.message_id, None)
        if action == "post_cancel" or not entry:
            await query.message.edit_text("🚫 the draft is dropped")
            await query.answer()
            return
        channel_id, draft = entry
        await self.bot.send_message(channel_id, draft)
        await query.message.edit_text("📮 published to the channel")
        await query.answer()

    async def cmd_status(self, message: Message, room: Room) -> None:
        st = await room.brain.status(self.bot.session)
        state = "running" if st.get("running") else "idle"
        await message.answer(f"🧠 backend {st.get('model', '?')} · {state} · turns {st.get('turns_done', 0)}")


def is_admin_of(app: BotApp, chat_id: int):
    def inner(message: Message) -> bool:
        return message.chat.id == chat_id and app.is_admin(chat_id, message.from_user.id)

    return inner


async def async_main(cfg: dict) -> None:
    app = BotApp(cfg)
    dp = Dispatcher()
    bot = app.bot

    # Every room's handlers are registered against its own chat id, so two
    # rooms cannot reach into each other's backends by accident.
    for chat_id, room in app.rooms.items():
        admin_here = lambda message, chat_id=chat_id: message.chat.id == chat_id and app.is_admin(
            chat_id, message.from_user.id
        )
        in_room = F.chat.id == chat_id

        @dp.message(Command("ask"), in_room, admin_here)
        async def on_ask(message: Message, command: CommandObject, room: Room = room) -> None:
            await app.cmd_ask(message, room, command.args or "")

        @dp.message(Command("post"), in_room, admin_here)
        async def on_post(message: Message, command: CommandObject, room: Room = room) -> None:
            if room.mode != "dev":
                await message.answer("the channel posts go through the developer room")
                return
            await app.cmd_post(message, room, command.args or "")

        @dp.message(Command("remember"), in_room, admin_here)
        async def on_remember(message: Message, command: CommandObject, room: Room = room) -> None:
            if room.mode != "dev":
                await message.answer("the vault is written in the developer room")
                return
            await app.cmd_remember(message, room, command.args or "")

        @dp.message(Command("status"), in_room, admin_here)
        async def on_status(message: Message, room: Room = room) -> None:
            await app.cmd_status(message, room)

        @dp.message(Command("cancel"), in_room, admin_here)
        async def on_cancel(message: Message, room: Room = room) -> None:
            await room.brain.cancel(bot.session)
            await message.answer("the turn is being stopped")

        @dp.message(Command("newsession"), in_room, admin_here)
        async def on_newsession(message: Message, room: Room = room) -> None:
            await room.brain.reset(bot.session)
            await message.answer("🧹 fresh session")

        @dp.message(Command("report"), in_room)
        async def on_report(message: Message, command: CommandObject, room: Room = room) -> None:
            if room.mode != "users":
                await message.answer("the reports are collected in the users' room")
                return
            await app.cmd_report(message, room, command.args or "")

        @dp.message(Command("digest"), in_room, admin_here)
        async def on_digest(message: Message, room: Room = room) -> None:
            await app.cmd_digest(message, room)

        @dp.message(in_room, F.text)
        async def on_room_message(message: Message, room: Room = room) -> None:
            who = message.from_user.full_name if message.from_user else "?"
            log.info("message in %s (%s) from %s id=%s", message.chat.id, room.name, who, message.from_user.id if message.from_user else "?")
            room.collector.add(who, message.text)
            if message.text.startswith("/"):
                return
            # Both rooms answer only when addressed: a reply to one of the
            # bot's messages. The users' room turns a reply into a report; the
            # dev room treats it as a question with the chat as context.
            if message.reply_to_message and message.reply_to_message.from_user.id == bot.id:
                if room.mode == "users":
                    await app.cmd_report(message, room, message.text)
                else:
                    await app.cmd_ask(message, room, message.text)


    @dp.callback_query(F.data.startswith("approve:"))
    async def on_approve(query: CallbackQuery) -> None:
        await app.handle_approve(query)

    @dp.callback_query(F.data.startswith("post_"))
    async def on_post_callback(query: CallbackQuery) -> None:
        await app.handle_post_callback(query)

    me = await bot.get_me()
    log.info("FlashAgent bot @%s is up over rooms: %s", me.username, ", ".join(f"{n}={r.chat_id}" for n, r in app.rooms.items()))
    await bot.delete_webhook(drop_pending_updates=True)
    await dp.start_polling(bot)


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(levelname)s %(message)s")
    path = pathlib.Path(__file__).parent / "bot.toml"
    cfg = load_config(path)
    asyncio.run(async_main(cfg))


if __name__ == "__main__":
    main()
