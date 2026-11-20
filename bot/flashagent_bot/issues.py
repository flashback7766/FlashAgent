"""The GitHub side of the report pipeline: search for dupes, file issues,
add comments. The token comes from the [github] block; rights are issues
plus moderation — the account cannot push code even if the bot goes mad."""

from __future__ import annotations

import httpx

API = "https://api.github.com"


class GithubError(Exception):
    """A GitHub call failed; the message is what the room gets to see."""


class GithubClient:
    def __init__(self, token: str, repo: str):
        self.token = token
        self.repo = repo  # "owner/name"
        self._headers = {
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        }

    async def search_open(
        self, http: httpx.AsyncClient, query: str, limit: int = 5
    ) -> list[dict]:
        """Open issues matching the words. GitHub's search is the only
        dedup tool that knows the whole repo."""
        q = f"repo:{self.repo} is:issue is:open {query}".strip()
        r = await http.get(
            f"{API}/search/issues",
            headers=self._headers,
            params={"q": q, "per_page": limit},
        )
        if r.status_code != 200:
            raise GithubError(f"поиск не вышел: HTTP {r.status_code}")
        return [
            {
                "number": i["number"],
                "title": i["title"],
                "url": i["html_url"],
                "body": (i.get("body") or "")[:500],
            }
            for i in r.json().get("items", [])
        ]

    async def closed_by_author_since(
        self, http: httpx.AsyncClient, author: str, since_iso: str, limit: int = 20
    ) -> list[dict]:
        """Issues CREATED by the bot account and closed since `since_iso`
        (ISO 8601, UTC). The search index lags a little; the watcher keeps
        its own `seen` set so a lag cannot double-report."""
        q = (
            f"repo:{self.repo} is:issue is:closed author:{author}"
            f" closed:>={since_iso}"
        )
        r = await http.get(
            f"{API}/search/issues",
            headers=self._headers,
            params={"q": q, "per_page": limit, "order": "desc"},
        )
        if r.status_code != 200:
            raise GithubError(f"поиск закрытых не вышел: HTTP {r.status_code}")
        return [
            {
                "number": i["number"],
                "title": i["title"],
                "url": i["html_url"],
                "closed_at": i.get("closed_at") or "",
            }
            for i in r.json().get("items", [])
        ]

    async def whoami(self, http: httpx.AsyncClient) -> str:
        """The login the token belongs to — the author the watcher watches."""
        r = await http.get(f"{API}/user", headers=self._headers)
        if r.status_code != 200:
            raise GithubError(f"кто я: HTTP {r.status_code}")
        return str(r.json().get("login") or "")

    async def create(
        self, http: httpx.AsyncClient, title: str, body: str
    ) -> dict:
        r = await http.post(
            f"{API}/repos/{self.repo}/issues",
            headers=self._headers,
            json={"title": title, "body": body},
        )
        if r.status_code not in (200, 201):
            raise GithubError(f"issue не создался: HTTP {r.status_code}")
        data = r.json()
        return {"number": data["number"], "url": data["html_url"]}

    async def comment(
        self, http: httpx.AsyncClient, number: int, body: str
    ) -> str:
        r = await http.post(
            f"{API}/repos/{self.repo}/issues/{number}/comments",
            headers=self._headers,
            json={"body": body},
        )
        if r.status_code not in (200, 201):
            raise GithubError(f"комментарий не лёг: HTTP {r.status_code}")
        return r.json()["html_url"]
