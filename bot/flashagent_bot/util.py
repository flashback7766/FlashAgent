"""Small text helpers shared by the Telegram layer."""

from __future__ import annotations


def escape_html(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


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
