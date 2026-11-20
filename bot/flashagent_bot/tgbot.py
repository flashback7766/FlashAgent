"""The Telegram side of the bot: rooms, commands, turns, the pulse.

One process serves the whole config. The bot is transport only: the
conversation context lives in each serve session, the background chatter
lives in journal files the brains read themselves. What the bot owns:
command routing, addressing rules, limits, budgets and the UX around
long-running turns.

Handler order matters: aiogram gives an update to the first matching
handler, so commands are registered before the catch-all room listener.
"""

from __future__ import annotations

import asyncio
import json
import logging
import time
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path

import httpx
from aiogram import Bot, Dispatcher, F, Router
from aiogram.client.default import DefaultBotProperties
from aiogram.enums import ChatAction, ParseMode
from aiogram.filters import Command, CommandObject, CommandStart
from aiogram.types import (
    CallbackQuery,
    InlineKeyboardButton,
    InlineKeyboardMarkup,
    Message,
    ReplyParameters,
)

from .brain import Event, ServeClient
from .config import Config, Room
from .issues import GithubClient, GithubError
from .journal import Journal
from .limits import ALLOW, COOLDOWN, MUTED, RateLimiter
from .posts import parse_series
from .usage import UsageStore
from .util import (
    escape_html,
    is_stub_reply,
    lang_hint,
    parse_dedup_decision,
    parse_issue_sentinel,
    search_keywords,
    split_reply,
)

log = logging.getLogger("flashagent_bot")

POLL_SECONDS = 1.0  # events poll period while a turn runs
RETRIES = 2  # automatic restarts after a failed turn
EDIT_THROTTLE = 2.0  # seconds between stage-note edits
ECONOMY_PER_HOUR = 5  # user turns per hour once the daily cap is hit
BREVITY = (
    "\n\n(Без преамбул и рекапов: не пересказывай состояние проекта, что ты"
    " делал в прошлых ходах и что лежит в папках — сразу ответ на вопрос,"
    " кратко и по делу. Отвечай на языке сообщения собеседника — обычно это"
    " русский, даже если служебные инструкции вокруг английские. Telegram не"
    " принимает сообщения длиннее 4096 символов.)"
)

ROOM_CONTEXT = {
    "dev": "ты в дев-группе проекта — тут разработчики, можно технично",
    "users": "ты в чате юзеров сообщества — пиши проще и дружелюбнее",
}

HELP = (
    "FlashAgent — бот проекта. Что я умею:\n"
    "/ask вопрос — спросить меня напрямую\n"
    "/report проблема — сообщить о баге или идее\n"
    "/status — что сейчас делает мозг\n"
    "/usage — расход токенов за сегодня (админы)\n"
    "/cancel — остановить текущий ответ (админы)\n"
    "/newsession — очистить контекст (админы)\n"
    "/remember факт — записать в память (админы)\n"
    "/post тема — черновик поста в канал (админы)\n"
    "/digest — сводка репортов юзеров (админы)\n"
    "Просто пиши в чат — я подключаюсь, когда адресован."
)


@dataclass
class RoomState:
    room: Room
    brain: ServeClient | None
    journal: Journal | None = None
    busy: bool = False
    pulse_mark: int = 0  # journal.count at the last pulse
    # One evolving message per target prompt: target msg id → the bot's
    # message that started as 💭 and becomes that prompt's answer. Lives for
    # one turn; steered prompts get theirs from the steer ack.
    slots: dict[int, int] = field(default_factory=dict)


@dataclass
class Draft:
    """A channel post waiting for the buttons. One per dev room: the new
    draft replaces the old, whose buttons then answer «уже нет»."""

    message_id: int
    text: str  # the brain's HTML, sent to the channel as-is


class App:
    def __init__(self, cfg: Config):
        self.cfg = cfg
        self.usage = UsageStore(cfg.data_dir)
        self.limiter = RateLimiter(
            cfg.requests_per_hour, cfg.ignore_hours, cfg.data_dir / "limits.json"
        )
        self.rooms: dict[int, RoomState] = {}
        for room in cfg.rooms:
            brain = None
            journal = None
            if room.serve is not None:
                brain = ServeClient(room.serve.url, room.serve.token)
                if room.journal:
                    journal = Journal(Path(room.serve.dir) / room.journal)
            st = RoomState(room=room, brain=brain, journal=journal)
            st.pulse_mark = journal.count if journal else 0
            self.rooms[room.chat_id] = st
        self.bot: Bot | None = None
        self.http: httpx.AsyncClient | None = None
        self.me_id = 0
        self.me_username = ""
        self.economy_warned = False
        self.digests_done: set[str] = set()
        self.gh = (
            GithubClient(cfg.github.token, cfg.github.repo)
            if cfg.github is not None
            else None
        )
        # The closed-issues watcher: what it already reported, and from when
        # it looks — persisted, so a restart does not re-report or forget.
        self.gh_login: str | None = None
        self.gh_seen: set[int] = set()
        self.gh_last: datetime | None = None
        self._gh_state = cfg.data_dir / "gh-watch.json"
        try:
            raw = json.loads(self._gh_state.read_text(encoding="utf-8"))
            self.gh_seen = {int(n) for n in raw.get("seen", [])}
            if raw.get("last"):
                self.gh_last = datetime.fromisoformat(raw["last"])
        except (ValueError, KeyError, OSError):
            pass
        # The answers already delivered: target message id → the bot's
        # message that carries the answer. Persisted, so a restart neither
        # re-delivers an answered prompt nor loses an in-flight one.
        self.delivered: dict[int, int] = {}
        try:
            raw = json.loads(
                (cfg.data_dir / "replies.json").read_text(encoding="utf-8")
            )
            self.delivered = {int(k): int(v) for k, v in raw.items()}
        except (ValueError, OSError):
            pass
        # Fire-and-forget coroutines need a held reference, or the event loop
        # may collect them mid-flight.
        self._tasks: set[asyncio.Task] = set()

    def _save_delivered(self) -> None:
        try:
            self.cfg.data_dir.mkdir(parents=True, exist_ok=True)
            items = list(self.delivered.items())[-300:]
            self.delivered = dict(items)
            (self.cfg.data_dir / "replies.json").write_text(
                json.dumps(dict(items)), encoding="utf-8"
            )
        except OSError:
            log.exception("delivered-replies save failed")
        self.drafts: dict[int, Draft] = {}
        self.redo_awaited: set[int] = set()
        self.series_done: set[str] = set()
        self.series_warned: set[int] = set()

    def _spawn(self, coro) -> None:
        task = asyncio.create_task(coro)
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)

    # ---------- startup ----------

    async def run(self) -> None:
        self.bot = Bot(
            self.cfg.token, default=DefaultBotProperties(parse_mode=ParseMode.HTML)
        )
        self.http = httpx.AsyncClient(timeout=30.0)
        me = await self.bot.get_me()
        self.me_id, self.me_username = me.id, me.username or ""
        dp = Dispatcher()
        router = Router()
        self._register(router)
        dp.include_router(router)
        for st in self.rooms.values():
            if st.brain is not None and not await st.brain.health(self.http):
                log.warning("brain %s (%s) is not answering", st.room.name, st.brain.url)
            if st.brain is not None:
                stale = await st.brain.catch_up(self.http)
                if stale:
                    log.info(
                        "feed %s: %d stale event(s) from before the restart",
                        st.brain.url,
                        len(stale),
                    )
                # An in-flight answer the old process never polled is not
                # garbage: the members still have no reply. Deliver it late.
                for ev in stale:
                    if (
                        ev.type == "reply"
                        and ev.message_id not in self.delivered
                        and (ev.text or "").strip()
                    ):
                        log.info("delivering in-flight reply to msg:%s", ev.message_id)
                        await self._deliver_reply(st, st.room.chat_id, ev.message_id, ev.text)
        if self.gh is not None:
            try:
                self.gh_login = await self.gh.whoami(self.http)
                log.info("github: watching as %s", self.gh_login)
            except GithubError as e:
                log.warning("github whoami failed: %s", e)
            if self.gh_last is None:
                self.gh_last = datetime.now(timezone.utc) - timedelta(days=1)
        background = [
            asyncio.create_task(self._pulse_loop()),
            asyncio.create_task(self._digest_loop()),
            asyncio.create_task(self._series_loop()),
            asyncio.create_task(self._gh_loop()),
        ]
        log.info("bot up as @%s, %d rooms", self.me_username, len(self.rooms))
        try:
            await dp.start_polling(self.bot)
        finally:
            for t in background:
                t.cancel()
            await self.http.aclose()

    def _register(self, router: Router) -> None:
        # Commands first — the catch-all listener comes last or it eats them.
        router.message.register(self.cmd_start, CommandStart())
        router.message.register(self.cmd_help, Command("help"))
        router.message.register(self.cmd_ask, Command("ask"))
        router.message.register(self.cmd_remember, Command("remember"))
        router.message.register(self.cmd_post, Command("post"))
        router.message.register(self.cmd_status, Command("status"))
        router.message.register(self.cmd_usage, Command("usage"))
        router.message.register(self.cmd_cancel, Command("cancel"))
        router.message.register(self.cmd_newsession, Command("newsession"))
        router.message.register(self.cmd_report, Command("report"))
        router.message.register(self.cmd_digest, Command("digest"))
        router.callback_query.register(self.on_approve, F.data.startswith("appr:"))
        router.callback_query.register(self.on_post_callback, F.data.startswith("post:"))
        router.message.register(self.on_room_message)

    # ---------- helpers ----------

    def state_by_chat(self, chat_id: int) -> RoomState | None:
        return self.rooms.get(int(chat_id))

    def is_admin(self, user_id: int | None) -> bool:
        return user_id is not None and int(user_id) in self.cfg.admins

    def dev_state(self) -> RoomState | None:
        return next((s for s in self.rooms.values() if s.room.kind == "dev"), None)

    def users_state(self) -> RoomState | None:
        return next((s for s in self.rooms.values() if s.room.kind == "users"), None)

    def over_budget(self) -> bool:
        return self.cfg.daily_tokens > 0 and self.usage.total() > self.cfg.daily_tokens

    def _who(self, message: Message) -> str:
        u = message.from_user
        if u is None:
            return "channel"
        name = u.first_name or u.username or "member"
        full = f"{name} (@{u.username})" if u.username else name
        # Who is speaking decides how the brain answers: the owner's word
        # weighs more than a member's question.
        if u.id == self.cfg.owner:
            role = "владелец"
        elif self.is_admin(u.id):
            role = "админ"
        else:
            role = "юзер"
        # The numeric id is what a tg://user?id= mention link is built from —
        # the way the model tags a member who has no @username.
        return f"[{role}] {full} (id:{u.id})"

    def _msg(self, message: Message) -> str:
        """The msg:N tag every turn prompt carries: the model's chat_reply
        answers this exact message, and the journal stores the same id."""
        return f"msg:{message.message_id} {self._who(message)}"

    def _addressed(self, st: RoomState, message: Message) -> bool:
        """A reply to the bot, an @mention, or a command that slipped here."""
        reply = message.reply_to_message
        if reply is not None and reply.from_user is not None:
            if reply.from_user.id == self.me_id:
                return True
        text = message.text or message.caption or ""
        return bool(self.me_username and f"@{self.me_username}" in text)

    async def _say(
        self, chat_id: int, text: str, reply_to: Message | None = None
    ) -> None:
        """Send an answer, split to fit Telegram. The first part replies to
        the member's message; the rest follow plainly — in forum groups the
        reply also lands the answer in the right topic."""
        assert self.bot is not None
        for i, part in enumerate(split_reply(escape_html(text))):
            try:
                if i == 0 and reply_to is not None:
                    await self.bot.send_message(
                        chat_id,
                        part,
                        reply_parameters=ReplyParameters(message_id=reply_to.message_id),
                    )
                else:
                    await self.bot.send_message(chat_id, part)
            except Exception:
                # A stale reply target must not eat the answer.
                await self.bot.send_message(chat_id, part)

    async def _ask_approval(self, chat_id: int, what: str) -> None:
        assert self.bot is not None
        keyboard = InlineKeyboardMarkup(
            inline_keyboard=[
                [
                    InlineKeyboardButton(text="✅ Разрешить", callback_data="appr:1"),
                    InlineKeyboardButton(text="❌ Запретить", callback_data="appr:0"),
                ]
            ]
        )
        await self.bot.send_message(
            chat_id,
            f"🔒 мозг просит запустить <b>{escape_html(what)}</b>",
            reply_markup=keyboard,
        )

    def _maybe_warn_budget(self, chat_id: int) -> None:
        if self.over_budget():
            if not self.economy_warned:
                self.economy_warned = True
                if self.bot is not None:
                    asyncio.ensure_future(
                        self.bot.send_message(
                            chat_id,
                            "🛑 дневной кап токенов превышен: пульс выключен,"
                            f" юзеры ограничены {ECONOMY_PER_HOUR} ходами/час,"
                            " команды админов работают как обычно",
                        )
                    )
        else:
            self.economy_warned = False  # the day rolled over, or spend went down

    # ---------- turns ----------

    async def _run_turn(
        self,
        st: RoomState,
        prompt: str,
        chat_id: int,
        reply_to: Message | None = None,
        deliver: bool = True,
    ) -> tuple[str | None, int]:
        """One turn against the room's brain. ONE message per prompt: the
        message that starts as 💭 and shows 🔧 as tools run is the slot the
        answer lands in — edited in place, never deleted and re-sent.
        `chat_reply` events go out the moment they arrive, and each target
        message keeps exactly one evolving reply. Returns (final text,
        replies sent); the caller only handles the sentinel. `deliver=False`
        for service turns whose result the caller handles (drafts, pulse)."""
        assert self.bot is not None and self.http is not None and st.brain is not None
        if st.busy:
            await self._steer(st, prompt, chat_id, reply_to)
            return (None, 0)
        # The brain must know which room it is talking in: the dev group
        # gets the technical voice, the users' chat the simple one.
        prompt = f"({ROOM_CONTEXT.get(st.room.kind, '')}) {prompt}"
        st.busy = True
        status: Message | None = None
        answered: set[int] = set()
        answer: list[str] = []
        tokens = [0, 0]
        replies = 0
        last_edit = 0.0
        typing_at = 0.0
        poll_errors = 0
        try:
            status = await self.bot.send_message(
                chat_id,
                "💭 думаю…",
                reply_parameters=(
                    ReplyParameters(message_id=reply_to.message_id) if reply_to else None
                ),
            )
            if reply_to is not None:
                st.slots[reply_to.message_id] = status.message_id

            async def stage(text: str) -> None:
                if status is not None:
                    try:
                        await status.edit_text(text)
                    except Exception:
                        pass

            err = await st.brain.start_turn(self.http, prompt)
            if err:
                await stage(f"⚠️ {escape_html(err)}")
                status = None  # the error stays; the finally must not eat it
                return (None, replies)
            retries = 0
            while True:
                await asyncio.sleep(POLL_SECONDS)
                if time.monotonic() - typing_at > 4.5:
                    typing_at = time.monotonic()
                    try:
                        await self.bot.send_chat_action(chat_id, ChatAction.TYPING)
                    except Exception:
                        pass
                try:
                    events = await st.brain.poll(self.http)
                    poll_errors = 0
                except httpx.HTTPError:
                    poll_errors += 1
                    if poll_errors > 120:  # two minutes of silence = down
                        events = [Event("failed", text="мозг недоступен")]
                    else:
                        continue
                for ev in events:
                    if ev.type == "text":
                        answer.append(ev.text)
                    elif ev.type == "reply":
                        # Live: the room sees each answer as the model calls
                        # it; the slot morphs from 💭 into the answer. The
                        # first reply to a prompt IS the answer: later calls
                        # to the same message never clobber it — the model
                        # likes to follow its answer with a state report.
                        if ev.message_id in answered:
                            continue
                        if await self._deliver_reply(st, chat_id, ev.message_id, ev.text):
                            answered.add(ev.message_id)
                            replies += 1
                    elif ev.type == "tool":
                        now = time.monotonic()
                        if now - last_edit > EDIT_THROTTLE:
                            last_edit = now
                            await stage(f"🔧 {escape_html(ev.name)}")
                    elif ev.type == "approval":
                        await self._ask_approval(chat_id, ev.name)
                    elif ev.type == "usage":
                        tokens[0] += ev.prompt
                        tokens[1] += ev.completion
                    elif ev.type == "failed":
                        if retries < RETRIES:
                            retries += 1
                            await stage(
                                f"⚠️ {escape_html(ev.text)}"
                                f"\nповторяю ({retries}/{RETRIES})…"
                            )
                            err = await st.brain.start_turn(self.http, prompt)
                            if err:
                                await stage(f"⚠️ повтор не вышел: {escape_html(err)}")
                                status = None
                                return (None, replies)
                            break
                        await stage(f"⚠️ ход не удался: {escape_html(ev.text)}")
                        status = None
                        return (None, replies)
                else:
                    if any(ev.type == "done" for ev in events):
                        break
                    continue
                break

            result = "".join(answer).strip()
            self._maybe_warn_budget(chat_id)
            trigger = reply_to.message_id if reply_to else None
            if replies == 0 and deliver and result:
                # The slot becomes the answer: the room watched one message
                # go 💭 → 🔧 → text, no delete-and-resent.
                await self._deliver_text(chat_id, status, result)
                status = None
                return (result, replies)
            if (
                replies
                and deliver
                and result
                and trigger is not None
                and trigger not in answered
            ):
                # The model tool-replied somewhere else (or emitted stubs):
                # this prompt's own slot gets the final text instead of
                # silence and a "Deleted message".
                await self._deliver_text(chat_id, status, result)
                status = None
                return (result, replies)
            if replies and result:
                log.info(
                    "turn covered by %d reply(ies); final kept: %s",
                    replies,
                    result[:200],
                )
            return (result or None, replies)
        finally:
            st.busy = False
            st.slots.clear()
            if tokens[0] or tokens[1]:
                self.usage.add(st.room.name, tokens[0], tokens[1])
            if status is not None:
                try:
                    await status.delete()
                except Exception:
                    pass

    async def _deliver_reply(
        self, st: RoomState, chat_id: int, target: int, text: str
    ) -> bool:
        """One message per target prompt, for good: the first chat_reply
        fills the prompt's slot (the 💭 status, or a fresh reply), and the
        delivered pair is remembered across restarts — a later call to the
        same message is ignored, so nothing replaces a real answer. Returns
        False for stubs and repeats."""
        assert self.bot is not None
        if is_stub_reply(text):
            return False
        if target in self.delivered:
            return False
        parts = split_reply(text)
        slot_id = st.slots.get(target)
        if slot_id is None:
            try:
                msg = await self.bot.send_message(
                    chat_id,
                    parts[0],
                    reply_parameters=ReplyParameters(message_id=target),
                )
            except Exception:
                # The member's message may be gone; the answer still lands.
                try:
                    msg = await self.bot.send_message(chat_id, escape_html(parts[0]))
                except Exception:
                    log.exception("reply to msg:%s failed", target)
                    return False
            st.slots[target] = msg.message_id
            slot_id = msg.message_id
        else:
            try:
                await self.bot.edit_message_text(parts[0], chat_id=chat_id, message_id=slot_id)
            except Exception:
                # The brain's HTML did not pass the parser: unformatted, same place.
                try:
                    await self.bot.edit_message_text(
                        escape_html(parts[0]), chat_id=chat_id, message_id=slot_id
                    )
                except Exception:
                    log.exception("edit reply to msg:%s failed", target)
                    return False
        self.delivered[target] = slot_id
        self._save_delivered()
        for part in parts[1:]:
            try:
                await self.bot.send_message(chat_id, part)
            except Exception:
                await self.bot.send_message(chat_id, escape_html(part))
        return True

    async def _deliver_text(self, chat_id: int, status: Message, text: str) -> None:
        """Morph the status message into the final answer; a too-long answer
        keeps going as follow-up messages."""
        assert self.bot is not None
        parts = split_reply(text)
        try:
            await status.edit_text(parts[0])
        except Exception:
            try:
                await status.edit_text(escape_html(parts[0]))
            except Exception:
                log.exception("final edit failed")
                return
        for part in parts[1:]:
            try:
                await self.bot.send_message(chat_id, part)
            except Exception:
                await self.bot.send_message(chat_id, escape_html(part))

    async def _steer(
        self, st: RoomState, prompt: str, chat_id: int, reply_to: Message | None
    ) -> None:
        """A prompt while the brain answers is a steer by itself: the bot
        just posts /turn, serve flies it into the running answer, and the
        loop rides it in the same request as the next tool results. The ack
        message is the prompt's slot — it morphs into the answer when the
        running brain gets to it."""
        assert self.bot is not None and self.http is not None and st.brain is not None
        try:
            err = await st.brain.start_turn(self.http, prompt)
        except httpx.HTTPError:
            err = "мозг не отвечает"
        if err:
            await self.bot.send_message(
                chat_id,
                "🧠 мозг сейчас занят и не принял влетание — повтори через минуту",
                reply_parameters=(
                    ReplyParameters(message_id=reply_to.message_id) if reply_to else None
                ),
            )
            return
        if reply_to is not None:
            try:
                ack = await self.bot.send_message(
                    chat_id,
                    "⚡ влетело в текущий ответ",
                    reply_parameters=ReplyParameters(message_id=reply_to.message_id),
                )
                st.slots[reply_to.message_id] = ack.message_id
            except Exception:
                pass

    # ---------- commands ----------

    async def cmd_start(self, message: Message) -> None:
        await message.answer(HELP)

    async def cmd_help(self, message: Message) -> None:
        await message.answer(HELP)

    async def cmd_ask(self, message: Message, command: CommandObject) -> None:
        st = self.state_by_chat(message.chat.id)
        if st is None or st.brain is None:
            return
        question = (command.args or "").strip()
        if not question:
            await message.answer("как пользоваться: /ask <вопрос>")
            return
        prompt = f"{self._msg(message)} спрашивает в группе: {question}{lang_hint(question)}{BREVITY}"
        text, replies = await self._run_turn(st, prompt, message.chat.id, reply_to=message)
        if text:
            await self._answer_room(st, text, message.chat.id, message, replies)

    async def cmd_remember(self, message: Message, command: CommandObject) -> None:
        st = self.state_by_chat(message.chat.id)
        if st is None or st.brain is None or not self.is_admin(message.from_user.id if message.from_user else None):
            return
        fact = (command.args or "").strip()
        if not fact:
            await message.answer("как пользоваться: /remember <факт>")
            return
        prompt = f"{self._msg(message)} просит: запиши в валт памяти проекта: {fact}"
        text, replies = await self._run_turn(st, prompt, message.chat.id, reply_to=message)
        if text:
            await self._answer_room(st, text, message.chat.id, message, replies)

    async def cmd_post(self, message: Message, command: CommandObject) -> None:
        st = self.state_by_chat(message.chat.id)
        if st is None or st.room.kind != "dev" or not self.is_admin(
            message.from_user.id if message.from_user else None
        ):
            return
        topic = (command.args or "").strip()
        if not topic:
            await message.answer("как пользоваться: /post <о чём пост>")
            return
        prompt = self._post_prompt(st, f"Тема поста: {topic}")
        text, _ = await self._run_turn(
            st, prompt, message.chat.id, reply_to=message, deliver=False
        )
        if text:
            await self._show_draft(st, text)

    def _post_prompt(self, st: RoomState, what: str) -> str:
        return (
            "Не используй тул chat_reply — пост уходит по своему пайплайну,"
            " ответь готовым текстом."
            f" {what}. Напиши готовый пост в Telegram-канал проекта. Контекст —"
            f" журнал дев-комнаты {st.room.journal} (свежие строки). Живой"
            " разговорный тон, уместные эмодзи, HTML-разметка (<b>жирный</b>,"
            " <a href=\"...\">ссылки</a>). Кратко: до 3500 символов. В ответе"
            " — только сам пост, без преамбулы."
        )

    async def _show_draft(self, st: RoomState, text: str) -> None:
        """Put the draft in the dev room with the three buttons. The draft
        is shown as the brain wrote it (HTML); Telegram rejects malformed
        markup, so the display falls back to plain text rather than dying."""
        assert self.bot is not None
        keyboard = InlineKeyboardMarkup(
            inline_keyboard=[
                [
                    InlineKeyboardButton(text="✅ Публиковать", callback_data="post:pub"),
                    InlineKeyboardButton(text="✏️ Переделать", callback_data="post:red"),
                    InlineKeyboardButton(text="❌ Отмена", callback_data="post:can"),
                ]
            ]
        )
        try:
            msg = await self.bot.send_message(
                st.room.chat_id, f"📝 черновик:\n\n{text}", reply_markup=keyboard
            )
        except Exception:
            msg = await self.bot.send_message(
                st.room.chat_id,
                f"📝 черновик:\n\n{escape_html(text)}",
                reply_markup=keyboard,
            )
        old = self.drafts.get(st.room.chat_id)
        if old is not None and old.message_id != msg.message_id:
            try:
                await self.bot.edit_message_text(
                    "↩️ черновик заменён — свежий ниже",
                    chat_id=st.room.chat_id,
                    message_id=old.message_id,
                )
            except Exception:
                pass
        self.drafts[st.room.chat_id] = Draft(message_id=msg.message_id, text=text)

    async def on_post_callback(self, query: CallbackQuery) -> None:
        assert self.bot is not None and self.http is not None
        if not self.is_admin(query.from_user.id):
            await query.answer("решают админы", show_alert=True)
            return
        if query.message is None:
            return
        st = self.state_by_chat(query.message.chat.id)
        if st is None or st.room.kind != "dev":
            return
        action = (query.data or "").split(":")[1]
        draft = self.drafts.get(st.room.chat_id)
        if draft is None:
            await query.answer("черновика уже нет")
            return
        if action == "can":
            self.drafts.pop(st.room.chat_id, None)
            try:
                await query.message.edit_text("❌ черновик отменён")
            except Exception:
                pass
            await query.answer("отменено")
            return
        if action == "pub":
            channel = next(
                (s for s in self.rooms.values() if s.room.kind == "channel"), None
            )
            if channel is None:
                await query.answer("в конфиге нет канала", show_alert=True)
                return
            try:
                await self.bot.send_message(channel.room.chat_id, draft.text)
            except Exception:
                # The brain's HTML did not pass Telegram's parser — the post
                # still goes out, just without the markup.
                try:
                    await self.bot.send_message(
                        channel.room.chat_id, escape_html(draft.text)
                    )
                except Exception:
                    await query.answer("канал не принял пост", show_alert=True)
                    return
            self.drafts.pop(st.room.chat_id, None)
            try:
                await query.message.edit_text("✅ опубликовано в канал")
            except Exception:
                pass
            await query.answer("опубликовано")
            return
        if action == "red":
            self.redo_awaited.add(st.room.chat_id)
            await self.bot.send_message(
                st.room.chat_id,
                "✏️ напиши, что поменять, reply-ем на сообщение черновика",
            )
            await query.answer("жду замечания")

    async def cmd_status(self, message: Message) -> None:
        st = self.state_by_chat(message.chat.id)
        if st is None or st.brain is None or self.http is None:
            return
        try:
            s = await st.brain.status(self.http)
        except httpx.HTTPError:
            await message.answer("⚠️ мозг не отвечает")
            return
        lines = [
            f"🧠 {escape_html(str(s.get('model', '?')))} —"
            f" {'занят' if s.get('running') else 'свободен'}",
            f"ходов: {s.get('turns_done', 0)}, событий: {s.get('events', 0)}",
            f"сессия: {escape_html(str(s.get('session', '?')))}",
        ]
        if s.get("last_error"):
            lines.append(f"ошибка: {escape_html(str(s['last_error']))}")
        if self.over_budget():
            lines.append("🛑 режим экономии")
        await message.answer("\n".join(lines))

    async def cmd_usage(self, message: Message) -> None:
        if not self.is_admin(message.from_user.id if message.from_user else None):
            return
        data = self.usage.today()
        lines = ["📊 расход за сегодня:"]
        for room, v in sorted(data.items()):
            lines.append(
                f"  {room}: {v.get('prompt', 0):,} + {v.get('completion', 0):,}"
                f" = {v.get('prompt', 0) + v.get('completion', 0):,}"
            )
        if not data:
            lines.append("  пока пусто")
        total = self.usage.total(data)
        lines.append(f"итого: {total:,}")
        if self.cfg.daily_tokens:
            pct = total * 100 // self.cfg.daily_tokens
            lines.append(f"кап: {self.cfg.daily_tokens:,} ({pct}%)")
        await message.answer("\n".join(lines))

    async def cmd_cancel(self, message: Message) -> None:
        st = self.state_by_chat(message.chat.id)
        if st is None or st.brain is None or self.http is None or not self.is_admin(
            message.from_user.id if message.from_user else None
        ):
            return
        await st.brain.cancel(self.http)
        await message.answer("✋ стоп")

    async def cmd_newsession(self, message: Message) -> None:
        st = self.state_by_chat(message.chat.id)
        if st is None or st.brain is None or self.http is None or not self.is_admin(
            message.from_user.id if message.from_user else None
        ):
            return
        await st.brain.reset(self.http)
        await message.answer("🧹 контекст очищен (валт и журнал остались)")

    async def cmd_report(self, message: Message, command: CommandObject) -> None:
        st = self.state_by_chat(message.chat.id)
        if st is None or st.brain is None:
            return
        report = (command.args or "").strip()
        if not report:
            await message.answer("как пользоваться: /report <что сломалось или идея>")
            return
        who = self._who(message)
        if st.journal is not None:
            st.journal.append(who, report, kind="report", msg_id=message.message_id)
        if not self.is_admin(message.from_user.id if message.from_user else None):
            verdict, secs = self.limiter.check(
                message.from_user.id,
                per_hour=ECONOMY_PER_HOUR if self.over_budget() else None,
            )
            if verdict == MUTED:
                await message.answer(f"😴 тишина ещё {int(secs // 3600)} ч {int(secs % 3600 // 60)} мин: был спам после предупреждения")
                return
            if verdict == COOLDOWN:
                await message.answer(f"⏳ лимит {self.cfg.requests_per_hour}/час, репорт приму через {int(secs // 60) + 1} мин")
                return
        prompt = (
            f"msg:{message.message_id} Пришёл репорт от {who}: «{report}»."
            " Разберись: задай уточняющие вопросы, если что-то неясно (шаги,"
            " логи, скриншоты) — каждый свой ответ посылай тула chat_reply"
            " на это сообщение. Когда картина ясна — проблема понятна, детали"
            " собраны — закончи ответ строкой ровно вида"
            " ISSUE: <короткий заголовок>, и issue будет заведён. Отвечай"
            " кратко."
        )
        text, replies = await self._run_turn(st, prompt, message.chat.id, reply_to=message)
        if text:
            await self._answer_room(st, text, message.chat.id, message, replies)

    async def cmd_digest(self, message: Message) -> None:
        st = self.state_by_chat(message.chat.id)
        if st is None or st.room.kind != "dev" or not self.is_admin(
            message.from_user.id if message.from_user else None
        ):
            return
        await self._digest(message.chat.id)

    async def _digest(self, chat_id: int) -> None:
        users = self.users_state()
        if users is None or users.room.journal is None or users.room.serve is None:
            if self.bot is not None:
                await self.bot.send_message(chat_id, "в конфиге нет юзер-комнаты с журналом")
            return
        path = Path(users.room.serve.dir) / users.room.journal
        prompt = (
            "Не используй тул chat_reply — это внутренняя сводка, ответь"
            f" обычным текстом. Сделай сводку репортов юзеров: прочитай {path}"
            " (свежие строки), собери баги и идеи по темам, отметь, что стоит"
            " завести в issues. Кратко, без воды."
        )
        dev = self.dev_state()
        if dev is None or dev.brain is None:
            return
        await self._run_turn(dev, prompt, chat_id)

    # ---------- the catch-all ----------

    async def on_room_message(self, message: Message) -> None:
        st = self.state_by_chat(message.chat.id)
        if st is None:
            # The id discovery path: add the bot to a room, send anything,
            # read the id here, put it in bot.toml.
            log.info(
                "message in an unconfigured chat %s (%s) — add it to bot.toml",
                message.chat.id,
                message.chat.type,
            )
            return
        room = st.room
        if room.kind == "channel":
            return  # broadcast only; nothing to listen to here

        if message.photo:
            await self._on_photo(st, message)
            if not message.caption:
                return

        text = (message.text or message.caption or "").strip()
        if not text:
            return
        who = self._who(message)
        if st.journal is not None:
            st.journal.append(who, text, msg_id=message.message_id)

        user_id = message.from_user.id if message.from_user else None
        if room.kind == "users" and not self.is_admin(user_id):
            verdict, secs = self.limiter.check(
                int(user_id), per_hour=ECONOMY_PER_HOUR if self.over_budget() else None
            )
            if verdict == MUTED:
                await message.answer(
                    f"😴 тишина ещё {int(secs // 3600)} ч {int(secs % 3600 // 60)} мин:"
                    " был спам после предупреждения"
                )
                return
            if verdict == COOLDOWN:
                await message.answer(
                    f"⏳ лимит {self.cfg.requests_per_hour}/час,"
                    f" отвечу через {int(secs // 60) + 1} мин"
                )
                return

        if room.kind == "dev" and room.chat_id in self.redo_awaited:
            reply = message.reply_to_message
            draft = self.drafts.get(room.chat_id)
            if (
                draft is not None
                and reply is not None
                and reply.message_id == draft.message_id
            ):
                # The critique rides on the draft itself: the next draft
                # replaces the old one, buttons and all.
                self.redo_awaited.discard(room.chat_id)
                prompt = (
                    f"Вот черновик поста:\n\n{draft.text}\n\nЗамечания от"
                    f" {who}: {text}\nПерепиши пост с учётом замечаний. В"
                    " ответе — только сам пост."
                )
                await message.answer("✏️ переделываю…")
                new_text, _ = await self._run_turn(
                    st, prompt, room.chat_id, deliver=False
                )
                if new_text:
                    await self._show_draft(st, new_text)
                return

        if room.kind == "dev" and not self._addressed(st, message):
            return  # background chatter: journalled, not answered
        prompt = f"{self._msg(message)}: {text}{lang_hint(text)}{BREVITY}"
        text_out, replies = await self._run_turn(st, prompt, message.chat.id, reply_to=message)
        if text_out:
            await self._answer_room(st, text_out, message.chat.id, message, replies)

    async def _answer_room(
        self,
        st: RoomState,
        text: str,
        chat_id: int,
        reply_to: Message | None,
        replies: int = 0,
    ) -> None:
        """The sentinel bookkeeping. The answer itself already went out —
        through the reply slots (replies > 0) or as the status message
        morphing into it (replies == 0). The `ISSUE:` line is the brain's
        order to file, not something the room reads."""
        assert self.bot is not None
        clean, issue_title = parse_issue_sentinel(text)
        if replies and clean:
            log.info("final covered by replies, kept: %s", clean[:200])
        if issue_title:
            if self.gh is None:
                await self.bot.send_message(
                    chat_id, "📌 [github] в bot.toml не настроен — issue не завёл"
                )
            else:
                self._spawn(self._file_issue(st, issue_title, chat_id))

    def _journal_tail(self, st: RoomState, n: int) -> str:
        if st.journal is None:
            return "(журнал пуст)"
        lines = [
            f"{r.get('who', '?')}: {r.get('text', '')}" for r in st.journal.tail(n)
        ]
        return "\n".join(lines) or "(журнал пуст)"

    async def _file_issue(self, st: RoomState, title: str, chat_id: int) -> None:
        """The report pipeline: the users' brain decided the problem is
        clear (the sentinel), so dedup against the repo and file or amend.
        The dev brain — the one that knows the project — writes the body."""
        assert self.http is not None and self.gh is not None
        dev = self.dev_state()
        if dev is None or dev.brain is None:
            return
        try:
            target: int | None = None
            candidates = await self.gh.search_open(
                self.http, search_keywords(title)
            )
            if candidates:
                listing = "\n".join(
                    f"#{c['number']} — {c['title']}: {c['body'][:200]}"
                    for c in candidates
                )
                prompt = (
                    "Не используй тул chat_reply — это внутреннее решение,"
                    " ответь обычным текстом. Юзер прислал проблему:"
                    f" «{title}». В репо есть открытые issues:\n{listing}\n"
                    "Если одно из них уже про то же самое — ответь ровно"
                    " REPLY: <номер>. Если проблема новая — ответь ровно NEW."
                )
                decision, _ = await self._run_turn(
                    dev, prompt, dev.room.chat_id, deliver=False
                )
                if decision:
                    target = parse_dedup_decision(decision)
            body_prompt = (
                "Не используй тул chat_reply — это внутренний черновик,"
                " ответь обычным текстом. Напиши тело issue для GitHub по"
                f" заголовку «{title}». Контекст — последние сообщения"
                " юзер-комнаты:\n"
                + self._journal_tail(st, 30)
                + "\nФормат: что происходит, шаги воспроизведения, ожидаемое"
                " поведение, окружение (версия приложения, устройство, если"
                " известно). Пиши по-русски, кратко и по делу."
            )
            body, _ = await self._run_turn(
                dev, body_prompt, dev.room.chat_id, deliver=False
            )
            body = body or title
            if target is not None:
                url = await self.gh.comment(
                    self.http, target, f"{body}\n\n— дополнение из чата юзеров"
                )
                await self._say(chat_id, f"📌 дополнил issue #{target}: {url}")
            else:
                issue = await self.gh.create(self.http, title, body)
                await self._say(chat_id, f"📌 завёл issue #{issue['number']}: {issue['url']}")
        except GithubError as e:
            await self._say(chat_id, f"⚠️ GitHub не вышел: {e}")

    async def _on_photo(self, st: RoomState, message: Message) -> None:
        """Screenshots land in the users' serve dir; the journal records
        where, so the brain can open them when discussing the problem."""
        assert self.bot is not None
        if st.room.serve is None or st.room.serve.dir is None or message.from_user is None:
            return
        name = f"{time.strftime('%Y%m%d-%H%M%S')}_{message.from_user.id}.jpg"
        dest = Path(st.room.serve.dir) / "shots" / name
        try:
            dest.parent.mkdir(parents=True, exist_ok=True)
            await self.bot.download(message.photo[-1].file_id, destination=str(dest))
        except Exception:
            log.exception("photo download failed")
            return
        rel = f"shots/{name}"
        who = self._who(message)
        if st.journal is not None:
            st.journal.append(
                who, f"📷 скриншот: {rel}", kind="photo", msg_id=message.message_id
            )
        await message.answer("📷 скрин сохранил — опиши словами, что не так")

    # ---------- callbacks ----------

    async def on_approve(self, query: CallbackQuery) -> None:
        assert self.http is not None
        if not self.is_admin(query.from_user.id):
            await query.answer("решают админы", show_alert=True)
            return
        if query.message is None:
            return
        st = self.state_by_chat(query.message.chat.id)
        if st is None or st.brain is None:
            return
        allow = (query.data or "").endswith(":1")
        await st.brain.approve(self.http, allow)
        await query.answer("разрешено" if allow else "запрещено")

    # ---------- background loops ----------

    async def _pulse_loop(self) -> None:
        """The dev room's active-participant pulse: after enough minutes of
        quiet and new journal lines, the brain reads the room's chatter and
        either speaks up or answers SKIP."""
        while True:
            await asyncio.sleep(60)
            try:
                now = time.monotonic()
                if not self.over_budget():
                    self.economy_warned = False
                for st in self.rooms.values():
                    if (
                        st.room.kind != "dev"
                        or not st.room.pulse_minutes
                        or st.journal is None
                        or st.brain is None
                        or st.busy
                        or self.over_budget()
                    ):
                        continue
                    if st.journal.count <= st.pulse_mark:
                        continue
                    if st.journal.quiet_for(now) < st.room.pulse_minutes * 60:
                        continue
                    st.pulse_mark = st.journal.count
                    await self._pulse_once(st)
            except Exception:
                log.exception("pulse loop hiccup")

    async def _pulse_once(self, st: RoomState) -> None:
        assert st.room.journal and st.brain is not None
        prompt = (
            "Периодический пульс: в группе прошла болтовня. Прочитай журнал"
            f" {st.room.journal} (свежие строки; в каждой есть id сообщения)"
            " и реши, есть ли что сказать по делу — незаданный вопрос,"
            " подсказка, предложение. Не занимайся отчетностью: не проверяй"
            " systemctl и сервисы, не описывай состояние машины, бота или"
            " своих возможностей — только разговор по делу из болтовни. Если"
            " есть что сказать — ответь тула chat_reply на конкретные"
            " сообщения (можно несколько вызовов). Если сказать нечего —"
            " ответь ровно одним словом SKIP обычным текстом, без тула."
        )
        text, replies = await self._run_turn(
            st, prompt, st.room.chat_id, deliver=False
        )
        if not replies and text and text.strip().upper() != "SKIP":
            await self._say(st.room.chat_id, text.strip())

    async def _digest_loop(self) -> None:
        """The users' room morning digest: once a day at the configured
        time, yesterday's reports get summarised into the dev room."""
        while True:
            await asyncio.sleep(30)
            try:
                now = time.localtime()
                for st in self.rooms.values():
                    t = st.room.morning_digest
                    if not t:
                        continue
                    hh, mm = (int(p) for p in t.split(":"))
                    if now.tm_hour != hh or now.tm_min != mm:
                        continue
                    key = f"{st.room.chat_id}-{time.strftime('%Y-%m-%d')}"
                    if key in self.digests_done:
                        continue
                    self.digests_done.add(key)
                    dev = self.dev_state()
                    if dev is None:
                        continue
                    await self._digest(dev.room.chat_id)
            except Exception:
                log.exception("digest loop hiccup")

    async def _series_loop(self) -> None:
        """The channel series (the devlog): at the configured weekday and
        time the dev brain gathers the week from the journal and posts a
        draft into the dev room — the buttons decide, never the schedule."""
        while True:
            await asyncio.sleep(60)
            try:
                now = time.localtime()
                for st in self.rooms.values():
                    spec = st.room.devlog
                    if not spec:
                        continue
                    parsed = parse_series(spec)
                    if parsed is None:
                        if st.room.chat_id not in self.series_warned:
                            self.series_warned.add(st.room.chat_id)
                            log.warning(
                                "devlog %r is unreadable: want 'Mon 18:00'", spec
                            )
                        continue
                    day, (hh, mm) = parsed
                    if now.tm_wday != day or now.tm_hour != hh or now.tm_min != mm:
                        continue
                    key = f"{st.room.chat_id}-{time.strftime('%Y-%m-%d')}"
                    if key in self.series_done:
                        continue
                    self.series_done.add(key)
                    prompt = self._post_prompt(
                        st,
                        "Собери девлог за последнюю неделю: что делали (из"
                        " журнала дев-комнаты), что выкатили, что дальше.",
                    )
                    text, _ = await self._run_turn(
                        st, prompt, st.room.chat_id, deliver=False
                    )
                    if text:
                        await self._show_draft(st, text)
            except Exception:
                log.exception("series loop hiccup")

    def _gh_save(self) -> None:
        try:
            self._gh_state.parent.mkdir(parents=True, exist_ok=True)
            self._gh_state.write_text(
                json.dumps(
                    {
                        "last": self.gh_last.isoformat() if self.gh_last else "",
                        "seen": sorted(self.gh_seen)[-200:],
                    },
                    ensure_ascii=False,
                ),
                encoding="utf-8",
            )
        except OSError:
            log.exception("gh-watch save failed")

    async def _gh_loop(self) -> None:
        """The closed-issues watcher: every couple of minutes the issues the
        bot account created get checked, and a closure lands in the dev room
        as a one-line notice."""
        assert self.bot is not None
        while True:
            await asyncio.sleep(150)
            if self.gh is None or self.http is None or self.gh_login is None:
                continue
            dev = self.dev_state()
            if dev is None:
                continue
            try:
                since = (
                    self.gh_last or datetime.now(timezone.utc) - timedelta(days=1)
                ).strftime("%Y-%m-%dT%H:%M:%SZ")
                items = await self.gh.closed_by_author_since(
                    self.http, self.gh_login, since
                )
                fresh = [i for i in items if i["number"] not in self.gh_seen]
                for i in sorted(fresh, key=lambda x: x["number"]):
                    self.gh_seen.add(i["number"])
                    await self.bot.send_message(
                        dev.room.chat_id,
                        f"📌 issue #{i['number']} «{escape_html(i['title'])}» закрыт: {i['url']}",
                    )
                self.gh_last = datetime.now(timezone.utc)
                if fresh:
                    self._gh_save()
            except GithubError as e:
                log.warning("gh watch: %s", e)
            except Exception:
                log.exception("gh loop hiccup")
