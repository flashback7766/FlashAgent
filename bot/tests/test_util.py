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
