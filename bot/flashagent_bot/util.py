"""Small text helpers shared by the Telegram layer."""

from __future__ import annotations

import re


def escape_html(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


_ALLOWED_TAGS = {"b", "i", "u", "s", "code", "pre"}
_TAG_RE = re.compile(r"&lt;(/?)([a-zA-Z][a-zA-Z0-9]*)((?:\s[^&<>]*?)?)&gt;")


def sanitize_tg_html(text: str) -> str:
    """The model writes Telegram HTML and likes to slip in a stray '<', and
    Telegram rejects the whole message for one bad tag. Escape everything,
    then let back only the handful of tags Telegram renders — an unknown or
    unparseable tag becomes plain text instead of a failed send."""
    escaped = escape_html(text)

    def restore(m: "re.Match[str]") -> str:
        slash, name, attrs = m.group(1), m.group(2).lower(), m.group(3)
        if name in _ALLOWED_TAGS:
            return f"<{slash}{name}{attrs}>"
        return ""  # a tag Telegram does not render: drop it, keep the text

    return _TAG_RE.sub(restore, escaped)


def plain_text(text: str) -> str:
    """The message with every tag stripped and escaped — the fallback when
    even the sanitized markup does not parse: readable, no literal junk."""
    return escape_html(re.sub(r"<[^>]*>", "", text))


def split_reply(text: str, limit: int = 3900) -> list[str]:
    """Cut a long answer into Telegram-sized chunks, preferring paragraph
    and line boundaries over mid-word cuts. 3900 leaves room for the HTML
    entities and markup Telegram charges against its 4096."""
    text = text.strip()
    if len(text) <= limit:
        return [text] if text else []
    parts: list[str] = []
    while text:
        if len(text) <= limit:
            parts.append(text)
            break
        cut = text.rfind("\n\n", 1, limit)
        if cut == -1:
            cut = text.rfind("\n", 1, limit)
        if cut == -1:
            cut = text.rfind(" ", 1, limit)
        if cut <= 0:
            cut = limit
        parts.append(text[:cut].strip())
        text = text[cut:].strip()
    return [p for p in parts if p]


def lang_hint(text: str) -> str:
    """The bot mirrors the member's language. Models mostly do this on
    their own; the hint only fires when the text is clearly non-Cyrillic
    Latin, so a Russian message never gets an English answer by accident."""
    cyr = sum(1 for ch in text if ("а" <= ch.lower() <= "я" or ch.lower() == "ё"))
    lat = sum(1 for ch in text if "a" <= ch.lower() <= "z")
    if lat >= 10 and lat > cyr * 2:
        return "\n\n(The member wrote in English — answer in English.)"
    return ""


ISSUE_PREFIX = "ISSUE:"

_ECHO_RE = None


def is_stub_reply(text: str) -> bool:
    """A chat_reply that says nothing: bare punctuation, or the tool's own
    result echoed back by the model ("Reply sent to msg:163." as a reply —
    the one thing a slot must never be replaced by)."""
    global _ECHO_RE
    stripped = (text or "").strip().strip("*_`")
    if not stripped or stripped in {".", "…", "-", "—", "•"}:
        return True
    if _ECHO_RE is None:
        import re

        _ECHO_RE = re.compile(
            r"^(?:reply sent to|replied to|sent to)\s+msg:\d+\.?$", re.IGNORECASE
        )
    return bool(_ECHO_RE.match(stripped))


def parse_issue_sentinel(text: str) -> tuple[str, str | None]:
    """The users' brain ends a report discussion it considers clear with a
    bare `ISSUE: <title>` line: that line is the bot's order to file the
    issue, not something the room should read. Returns the text without
    the sentinel and the title, or None when the brain said nothing yet."""
    kept: list[str] = []
    title: str | None = None
    for line in text.splitlines():
        stripped = line.strip()
        if title is None and stripped.startswith(ISSUE_PREFIX):
            title = stripped[len(ISSUE_PREFIX):].strip()
            continue
        kept.append(line)
    return "\n".join(kept).strip(), (title or None)


def parse_dedup_decision(text: str) -> int | None:
    """The dev brain's dedup verdict: `REPLY: 12` amends issue #12, `NEW`
    (or anything unreadable) files a new one — a wrong "new" is cheaper
    than a wrongly amended issue."""
    for line in text.splitlines():
        s = line.strip().upper()
        if s.startswith("REPLY"):
            digits = "".join(ch for ch in s if ch.isdigit())
            return int(digits) if digits else None
        if s.startswith("NEW"):
            return None
    return None


def search_keywords(title: str, limit: int = 6) -> str:
    """The words of a title as a GitHub search query: punctuation gone,
    short words gone, a handful of terms — the search does the rest."""
    words = [w.strip(".,!?«»\"';:") for w in title.split()]
    kept = [w for w in words if len(w) > 2]
    return " ".join(kept[:limit])
