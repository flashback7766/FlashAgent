"""Text helpers: chunking to Telegram's limit and the language mirror."""

from flashagent_bot.util import lang_hint, split_reply


def test_short_text_stays_whole():
    assert split_reply("привет") == ["привет"]


def test_empty_text_is_no_parts():
    assert split_reply("   ") == []


def test_long_text_splits_under_the_limit():
    text = "строка\n" * 800  # 4800 chars
    parts = split_reply(text, limit=3900)
    assert all(len(p) <= 3900 for p in parts)
    # nothing lost: the non-newline content is fully covered by the chunks
    assert sum(len(p.replace("\n", "")) for p in parts) == len(
        text.replace("\n", "").strip()
    )


def test_prefers_paragraph_boundaries():
    text = ("a" * 2000 + "\n\n" + "b" * 2000 + "\n\n" + "c" * 2000)
    parts = split_reply(text, limit=3900)
    assert len(parts) >= 2
    assert parts[0].endswith("a" * 10)  # cut at the paragraph edge, not mid-word


def test_hard_cut_without_boundaries():
    text = "x" * 8000
    parts = split_reply(text, limit=3900)
    assert len(parts) == 3  # 3900 + 3900 + 200
    assert "".join(parts) == text


def test_english_gets_a_hint():
    assert lang_hint("please help me with the build system here") != ""


def test_russian_gets_no_hint():
    assert lang_hint("помоги собрать проект, тут всё сломалось") == ""


def test_short_latin_is_ignored():
    # a mention or a filename must not flip the whole answer to English
    assert lang_hint("а что с main.rs?") == ""


from flashagent_bot.util import is_stub_reply


def test_tool_result_echo_is_a_stub():
    # The model echoes the chat_reply tool result back as a "reply" — the
    # one text a slot must never be replaced by.
    assert is_stub_reply("Reply sent to msg:163.")
    assert is_stub_reply("Replied to msg:42.")
    assert is_stub_reply("reply sent to msg:7")


def test_real_answers_are_not_stubs():
    assert not is_stub_reply("Alive and hooked up — give me a task.")
    assert not is_stub_reply("Сделал: склонировал репо в brain/rx-playa.")
    assert is_stub_reply(".")
    assert is_stub_reply("—")
    assert is_stub_reply("   ")
