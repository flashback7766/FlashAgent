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
import logging
import time
from dataclasses import dataclass, field
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
from .usage import UsageStore
from .util import (
    escape_html,
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
    "\n\n(Ответь кратко и по делу: Telegram не принимает сообщения"
    " длиннее 4096 символов.)"
)

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
        # Fire-and-forget coroutines need a held reference, or the event loop
        # may collect them mid-flight.
        self._tasks: set[asyncio.Task] = set()

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
        background = [
            asyncio.create_task(self._pulse_loop()),
            asyncio.create_task(self._digest_loop()),
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
        role = "admin" if self.is_admin(u.id) else "member"
        return f"[{role}] {full}"

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
    ) -> str | None:
        """One turn against the room's brain. The stage note is edited as
        tools run, the finished answer goes out in chunks. Returns the
        final text (the pulse greps it for SKIP) or None on failure.
        When the brain is already thinking, the prompt flies into the
        running turn as a steer instead of queueing behind it."""
        assert self.bot is not None and self.http is not None and st.brain is not None
        if st.busy:
            await self._steer(st, prompt, chat_id, reply_to)
            return None
        st.busy = True
        note = await self.bot.send_message(
            chat_id,
            "💭 думаю…",
            reply_parameters=(
                ReplyParameters(message_id=reply_to.message_id) if reply_to else None
            ),
        )
        answer: list[str] = []
        tokens = [0, 0]
        last_edit = 0.0
        typing_at = 0.0
        poll_errors = 0
        result: str | None = None
        try:
            err = await st.brain.start_turn(self.http, prompt)
            if err:
                await note.edit_text(f"⚠️ {escape_html(err)}")
                return None
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
                    elif ev.type == "tool":
                        now = time.monotonic()
                        if now - last_edit > EDIT_THROTTLE:
                            last_edit = now
                            try:
                                await note.edit_text(f"🔧 {escape_html(ev.name)}")
                            except Exception:
                                pass
                    elif ev.type == "approval":
                        await self._ask_approval(chat_id, ev.name)
                    elif ev.type == "usage":
                        tokens[0] += ev.prompt
                        tokens[1] += ev.completion
                    elif ev.type == "failed":
                        if retries < RETRIES:
                            retries += 1
                            try:
                                await note.edit_text(
                                    f"⚠️ {escape_html(ev.text)}"
                                    f"\nповторяю ({retries}/{RETRIES})…"
                                )
                            except Exception:
                                pass
                            err = await st.brain.start_turn(self.http, prompt)
                            if err:
                                await note.edit_text(f"⚠️ повтор не вышел: {escape_html(err)}")
                                return None
                            break
                        try:
                            await note.edit_text(f"⚠️ ход не удался: {escape_html(ev.text)}")
                        except Exception:
                            pass
                        return None
                else:
                    if any(ev.type == "done" for ev in events):
                        break
                    continue
                break
        finally:
            st.busy = False
            if tokens[0] or tokens[1]:
                self.usage.add(st.room.name, tokens[0], tokens[1])
            try:
                await note.delete()
            except Exception:
                pass
        result = "".join(answer).strip()
        self._maybe_warn_budget(chat_id)
        return result or None

    async def _steer(
        self, st: RoomState, prompt: str, chat_id: int, reply_to: Message | None
    ) -> None:
        assert self.bot is not None and self.http is not None and st.brain is not None
        try:
            err = await st.brain.steer(self.http, prompt)
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
        else:
            await self.bot.send_message(chat_id, "⚡ влетело в текущий ответ")

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
        prompt = f"{self._who(message)} спрашивает в группе: {question}{lang_hint(question)}{BREVITY}"
        text = await self._run_turn(st, prompt, message.chat.id, reply_to=message)
        if text:
            await self._say(message.chat.id, text, reply_to=message)

    async def cmd_remember(self, message: Message, command: CommandObject) -> None:
        st = self.state_by_chat(message.chat.id)
        if st is None or st.brain is None or not self.is_admin(message.from_user.id if message.from_user else None):
            return
        fact = (command.args or "").strip()
        if not fact:
            await message.answer("как пользоваться: /remember <факт>")
            return
        prompt = f"Запиши в валт памяти проекта: {fact}"
        text = await self._run_turn(st, prompt, message.chat.id, reply_to=message)
        if text:
            await self._say(message.chat.id, text, reply_to=message)

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
        # The draft-and-buttons pipeline lands with the channel phase; until
        # then the command says so instead of silently doing nothing.
        await message.answer(
            "📝 пайплайн канала ещё пишется — черновики с кнопками появятся в следующей фазе"
        )

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
            st.journal.append(who, report, kind="report")
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
            f"Пришёл репорт от {who}: «{report}». Разберись: задай уточняющие"
            " вопросы, если что-то неясно (шаги, логи, скриншоты). Когда"
            " картина ясна — проблема понятна, детали собраны — закончи ответ"
            " строкой ровно вида ISSUE: <короткий заголовок>, и issue будет"
            " заведён. Отвечай кратко."
        )
        text = await self._run_turn(st, prompt, message.chat.id, reply_to=message)
        if text:
            await self._answer_room(st, text, message.chat.id, reply_to=message)

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
            f"Сделай сводку репортов юзеров: прочитай {path} (свежие строки),"
            " собери баги и идеи по темам, отметь, что стоит завести в issues."
            " Кратко, без воды."
        )
        dev = self.dev_state()
        if dev is None or dev.brain is None:
            return
        text = await self._run_turn(dev, prompt, chat_id)
        if text:
            await self._say(chat_id, text)

    # ---------- the catch-all ----------

    async def on_room_message(self, message: Message) -> None:
        st = self.state_by_chat(message.chat.id)
        if st is None:
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
            st.journal.append(who, text)

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

        if room.kind == "dev" and not self._addressed(st, message):
            return  # background chatter: journalled, not answered
        prompt = f"{who}: {text}{lang_hint(text)}{BREVITY}"
        text_out = await self._run_turn(st, prompt, message.chat.id, reply_to=message)
        if text_out:
            await self._answer_room(st, text_out, message.chat.id, reply_to=message)

    async def _answer_room(
        self, st: RoomState, text: str, chat_id: int, reply_to: Message | None
    ) -> None:
        """Post a turn's answer and act on its sentinel. The `ISSUE:` line
        is the brain's order to file, not something the room reads."""
        assert self.bot is not None
        clean, issue_title = parse_issue_sentinel(text)
        if clean:
            await self._say(chat_id, clean, reply_to=reply_to)
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
                    f"Юзер прислал проблему: «{title}». В репо есть открытые"
                    f" issues:\n{listing}\nЕсли одно из них уже про то же"
                    " самое — ответь ровно REPLY: <номер>. Если проблема"
                    " новая — ответь ровно NEW."
                )
                decision = await self._run_turn(dev, prompt, dev.room.chat_id)
                if decision:
                    target = parse_dedup_decision(decision)
            body_prompt = (
                f"Напиши тело issue для GitHub по заголовку «{title}»."
                " Контекст — последние сообщения юзер-комнаты:\n"
                + self._journal_tail(st, 30)
                + "\nФормат: что происходит, шаги воспроизведения, ожидаемое"
                " поведение, окружение (версия приложения, устройство, если"
                " известно). Пиши по-русски, кратко и по делу."
            )
            body = (
                await self._run_turn(dev, body_prompt, dev.room.chat_id) or title
            )
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
            st.journal.append(who, f"📷 скриншот: {rel}", kind="photo")
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
            f" {st.room.journal} (свежие строки) и реши, есть ли что сказать"
            " по делу — незаданный вопрос, подсказка, предложение. Если есть,"
            " скажи в 1–3 предложениях. Если сказать нечего — ответь ровно"
            " одним словом SKIP."
        )
        text = await self._run_turn(st, prompt, st.room.chat_id)
        if text and text.strip().upper() != "SKIP":
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
