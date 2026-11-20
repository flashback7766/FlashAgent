"""FlashAgent, the Telegram face.

The bot sits in a group, listens to what people say, and is the one who talks
to the FlashAgent backend (`flashagent --serve`): asks it questions with the
chat as context, brings back answers, drafts channel posts for approval, and
puts facts into the memory vault. The brain does the work; this process only
carries messages and buttons.
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
    for section in ("telegram", "serve"):
        if section not in cfg:
            raise ConfigError(f"config {path} has no [{section}] section")
    if not cfg["telegram"].get("token"):
        raise ConfigError("config [telegram].token is empty: the BotFather token goes there")
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
    """The HTTP client for `flashagent --serve`. Polls, does not stream: a
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

    async def poll(self, session, poll_interval: float) -> list[ServeEvent]:
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


class ChatCollector:
    """The last messages of the group, the context every ask rides on."""

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
            return "(the group has been quiet)"
        return "\n".join(self.lines)


class RxPlaya:
    """The bot for one deployment: a group, a channel and one backend."""

    def __init__(self, cfg: dict):
        tg = cfg["telegram"]
        self.bot = Bot(tg["token"], default=DefaultBotProperties(parse_mode=ParseMode.HTML))
        self.group_id = int(tg["group_id"])
        self.channel_id = int(tg["channel_id"])
        self.admin_ids = set(tg.get("admin_ids", []))
        serve = cfg["serve"]
        self.brain = Brain(serve["url"], serve.get("token", ""))
        self.poll_interval = float(cfg.get("brain", {}).get("poll_interval", 1.0))
        self.collector = ChatCollector(int(cfg.get("brain", {}).get("context_messages", 30)))
        self.pending_post: dict[int, str] = {}

    def is_admin(self, user_id: int | None) -> bool:
        return user_id in self.admin_ids

    async def run_turn(self, message: Message, prompt: str) -> None:
        """One turn against the backend, with the chat as context. The live
        line is edited as tools run; the finished answer is sent in chunks."""
        note = await message.answer("💭 думаю…")
        ok, why = await self.brain.start_turn(self.bot.session, prompt)
        if not ok:
            await note.edit_text(f"⚠️ {why}")
            return
        answer = []
        last_edit = 0.0
        failed = None
        while True:
            await asyncio.sleep(self.poll_interval)
            events = await self.brain.poll(self.bot.session)
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
        if not self.is_admin(query.from_user.id):
            await query.answer("only the admins answer this", show_alert=True)
            return
        allow = query.data.endswith("1")
        await self.brain.approve(self.bot.session, allow)
        await query.answer("allowed" if allow else "denied")

    async def handle_post_callback(self, query: CallbackQuery) -> None:
        if not self.is_admin(query.from_user.id):
            await query.answer("only the admins publish", show_alert=True)
            return
        action = query.data
        draft = self.pending_post.pop(query.message.message_id, None)
        if action == "post_cancel" or not draft:
            await query.message.edit_text("🚫 the draft is dropped")
            await query.answer()
            return
        await self.bot.send_message(self.channel_id, draft)
        await query.message.edit_text("📮 published to the channel")
        await query.answer()

    async def cmd_ask(self, message: Message, command: CommandObject) -> None:
        question = (command.args or "").strip()
        if not question:
            await message.answer("usage: /ask <question>")
            return
        prompt = (
            "You are FlashAgent, sitting in a group chat. The recent chat:\n\n"
            f"{self.collector.context()}\n\n"
            f"A member asked: {question}\nAnswer them directly."
        )
        await self.run_turn(message, prompt)

    async def cmd_post(self, message: Message, command: CommandObject) -> None:
        topic = (command.args or "").strip()
        if not topic:
            await message.answer("usage: /post <what the post is about>")
            return
        prompt = (
            "Draft a Telegram channel post. The recent group chat:\n\n"
            f"{self.collector.context()}\n\n"
            f"The post is about: {topic}\n"
            "Write the post itself, ready to publish, in the chat's language. No preamble."
        )
        note = await message.answer("📝 черновик пишется…")
        ok, why = await self.brain.start_turn(self.bot.session, prompt)
        if not ok:
            await note.edit_text(f"⚠️ {why}")
            return
        answer = []
        failed = None
        while True:
            await asyncio.sleep(self.poll_interval)
            events = await self.brain.poll(self.bot.session)
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
        self.pending_post[sent.message_id] = draft

    async def cmd_remember(self, message: Message, command: CommandObject) -> None:
        fact = (command.args or "").strip()
        if not fact:
            await message.answer("usage: /remember <what to keep>")
            return
        await self.run_turn(
            message,
            f"Remember this fact in the memory vault, exactly as it was said, under a fitting name: {fact}",
        )

    async def cmd_status(self, message: Message) -> None:
        st = await self.brain.status(self.bot.session)
        state = "running" if st.get("running") else "idle"
        await message.answer(f"🧠 backend {st.get('model', '?')} · {state} · turns {st.get('turns_done', 0)}")


def is_admin_of(app: RxPlaya):
    def inner(message: Message) -> bool:
        return message.chat.id == app.group_id and app.is_admin(message.from_user.id)

    return inner


async def async_main(cfg: dict) -> None:
    app = RxPlaya(cfg)
    dp = Dispatcher()
    bot = app.bot

    admin_here = is_admin_of(app)

    @dp.message(F.chat.id == app.group_id, F.text)
    async def on_group_message(message: Message) -> None:
        who = message.from_user.full_name if message.from_user else "?"
        app.collector.add(who, message.text)
        if message.text.startswith("/"):
            return
        # The bot answers only when addressed: a reply to its own message.
        if message.reply_to_message and message.reply_to_message.from_user.id == bot.id:
            command = CommandObject(command="ask", args=message.text)
            await app.cmd_ask(message, command)

    @dp.message(Command("ask"), F.chat.id == app.group_id, admin_here)
    async def on_ask(message: Message, command: CommandObject) -> None:
        await app.cmd_ask(message, command)

    @dp.message(Command("post"), F.chat.id == app.group_id, admin_here)
    async def on_post(message: Message, command: CommandObject) -> None:
        await app.cmd_post(message, command)

    @dp.message(Command("remember"), F.chat.id == app.group_id, admin_here)
    async def on_remember(message: Message, command: CommandObject) -> None:
        await app.cmd_remember(message, command)

    @dp.message(Command("status"), F.chat.id == app.group_id, admin_here)
    async def on_status(message: Message) -> None:
        await app.cmd_status(message)

    @dp.message(Command("cancel"), F.chat.id == app.group_id, admin_here)
    async def on_cancel(message: Message) -> None:
        await app.brain.cancel(bot.session)
        await message.answer("the turn is being stopped")

    @dp.message(Command("newsession"), F.chat.id == app.group_id, admin_here)
    async def on_newsession(message: Message) -> None:
        await app.brain.reset(bot.session)
        await message.answer("🧹 fresh session")

    @dp.callback_query(F.data.startswith("approve:"))
    async def on_approve(query: CallbackQuery) -> None:
        await app.handle_approve(query)

    @dp.callback_query(F.data.startswith("post_"))
    async def on_post_callback(query: CallbackQuery) -> None:
        await app.handle_post_callback(query)

    me = await bot.get_me()
    log.info("FlashAgent bot @%s is up, group %s", me.username, app.group_id)
    await bot.delete_webhook(drop_pending_updates=True)
    await dp.start_polling(bot)


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(levelname)s %(message)s")
    path = pathlib.Path(__file__).parent / "bot.toml"
    cfg = load_config(path)
    asyncio.run(async_main(cfg))


if __name__ == "__main__":
    main()
