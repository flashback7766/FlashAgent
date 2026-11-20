"""HTTP client for one `flashagent --serve`. The bot polls, it does not
stream: a turn runs for minutes, a poll costs a second. Event shapes are
whatever serve.rs emits; unknown types are parsed loosely so a newer
serve does not break an older bot."""

from __future__ import annotations

from dataclasses import dataclass

import httpx


@dataclass
class Event:
    type: str
    text: str = ""
    name: str = ""
    reason: str = ""
    step: int = 0
    max_steps: int = 0
    prompt: int = 0
    completion: int = 0
    saved: int = 0


class ServeClient:
    def __init__(self, url: str, token: str):
        self.url = url.rstrip("/")
        self.token = token
        self.since = 0

    def _headers(self) -> dict[str, str]:
        return {"Authorization": f"Bearer {self.token}"} if self.token else {}

    async def health(self, http: httpx.AsyncClient) -> bool:
        try:
            r = await http.get(
                f"{self.url}/health", headers=self._headers(), timeout=5.0
            )
            return r.status_code == 200
        except httpx.HTTPError:
            return False

    async def status(self, http: httpx.AsyncClient) -> dict:
        r = await http.get(f"{self.url}/status", headers=self._headers())
        return r.json()

    async def start_turn(self, http: httpx.AsyncClient, text: str) -> str:
        """Hand the prompt to the brain. A prompt that lands while a turn
        runs is a steer by itself (serve flies it into the running answer).
        Returns "" on acceptance, the error text otherwise."""
        r = await http.post(
            f"{self.url}/turn", headers=self._headers(), json={"text": text}
        )
        if r.status_code == 200:
            return ""
        return self._error_of(r)

    async def approve(self, http: httpx.AsyncClient, allow: bool) -> None:
        await http.post(
            f"{self.url}/approve", headers=self._headers(), json={"allow": allow}
        )

    async def cancel(self, http: httpx.AsyncClient) -> None:
        await http.post(f"{self.url}/cancel", headers=self._headers())

    async def reset(self, http: httpx.AsyncClient) -> None:
        await http.post(f"{self.url}/reset", headers=self._headers())

    async def poll(self, http: httpx.AsyncClient) -> list[Event]:
        """New events since the last poll, oldest first."""
        r = await http.get(
            f"{self.url}/events",
            headers=self._headers(),
            params={"since": self.since},
        )
        body = r.json()
        events = body.get("events", [])
        self.since += len(events)
        out: list[Event] = []
        for ev in events:
            kind = str(ev.get("type", ""))
            if kind == "tool_start":
                out.append(Event("tool", name=str(ev.get("name", ""))))
            elif kind == "approval":
                args = ev.get("args_json")
                shown = args if isinstance(args, str) else ""
                out.append(
                    Event("approval", name=f"{ev.get('tool', '')} {shown}".strip())
                )
            elif kind == "turn_failed":
                out.append(Event("failed", text=str(body.get("last_error") or "unknown error")))
            else:
                out.append(
                    Event(
                        kind,
                        text=str(ev.get("text", "")),
                        name=str(ev.get("name", "") or ev.get("tool", "")),
                        reason=str(ev.get("reason", "")),
                        step=int(ev.get("step", 0) or 0),
                        max_steps=int(ev.get("max_steps", 0) or 0),
                        prompt=int(ev.get("prompt", 0) or 0),
                        completion=int(ev.get("completion", 0) or 0),
                        saved=int(ev.get("saved", 0) or 0),
                    )
                )
        return out

    def _error_of(self, r: httpx.Response) -> str:
        try:
            return str(r.json().get("error", f"HTTP {r.status_code}"))
        except ValueError:
            return f"HTTP {r.status_code}"
