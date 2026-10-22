# Security policy

## Supported versions

Fixes go into the next release; only the latest builds get them.

| Version                         | Supported          |
| ------------------------------- | ------------------ |
| Latest beta (`bNNN`, `beta` release) | :white_check_mark: |
| Latest stable (`vX.Y.Z+bN`, from v1.0.0) | :white_check_mark: |
| Older builds                    | :x:                |

Release downloads are listed in each release's `SHA256SUMS`; the in-app updater refuses a binary whose checksum does not match, and refuses a release that lists no checksum at all.

There is no code signature: the checksum comes from the same release, so it protects against a corrupted or swapped download, not against a compromised account. Run the app from a source checkout if that matters to you.

## Reporting a vulnerability

Examples: a way past the permission modes or approval cards, a shell command run without approval, a leaked key.

1. **Do not report it publicly** (issues, discussions, social media).
2. Open a [GitHub security advisory](https://github.com/flashback7766/FlashAgent/security/advisories/new), or write to the maintainer on Discord (`flashback7766`) or Telegram ([@flashback2k](https://t.me/flashback2k)).
3. Give the steps to reproduce it: your OS, version, and the commands or input used.

You get an answer within 48 hours, and the fix and its disclosure are agreed with you.
