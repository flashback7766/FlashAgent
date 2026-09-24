# Versioning

FlashAgent has two release channels and one version format for each. All
version logic lives in `crates/svc/src/version.rs`; the updater, the
channel-switch card and the what's-new screen use it and nothing else.

## Formats

| Channel | Format | Example | Meaning |
|---|---|---|---|
| Beta | `b<N>` | `b287` | Build counter. Only ever grows. |
| Stable | `v<MAJOR>.<MINOR>.<PATCH>+b<N>` | `v1.0.0+b290` | A SemVer release, cut from beta build `N`. |

- The `+b<N>` part uses SemVer build-metadata syntax. FlashAgent deliberately
  uses it for ordering, including between stable releases: the build counter
  takes precedence over `MAJOR.MINOR.PATCH`.
- When reading a version, the leading `v` is optional, `v1.2` means `v1.2.0`,
  and a stable version without `+b<N>` is accepted (see rule 4 below).
- Rolling release names (`beta`, `stable`, `release`, `latest`) are not
  versions. The real version of a rolling release is read from its title
  ("FlashAgent b287"), then its tag, asset names and notes.

## Ordering

One rule, used everywhere:

1. Every version with a build number compares by `N` first, including
   stable releases: `v2.0.0+b290 < v1.0.0+b291`.
2. Stables with the same build compare by `MAJOR.MINOR.PATCH`.
3. On a tied build a stable is
   newer, because it is that build, released: `b290 < v1.0.0+b290 < b291`.
4. A legacy stable without `+b<N>` is newer than every numbered build,
   including numbered stables. Two unnumbered stables compare by
   `MAJOR.MINOR.PATCH`. This keeps old hand-made releases readable and the
   ordering transitive; new releases must include the build number.

## What the app says

**Background updates** install the newest release on the chosen channel, and
only when it is newer than the running one. A background update never goes
backwards.

- **Stable channel**: stable releases only.
- **Beta channel**: betas *and* stable releases. The newest wins, so a beta
  install also moves to a stable release built after the beta it runs.

**Switching channel** (Settings → Updates, or `/channel`) asks first. The card
compares the running version with the newest release on the target channel,
using the ordering above, and says which it is:

- **Update: b287 → v1.0.0+b290**: the target is newer. You get everything
  added since.
- **Downgrade: b300 → v1.0.0+b290**: the target is older. Features added since
  may disappear, and settings they introduced can be reset.

The same wording is used in both directions. Beta → stable can be an update,
and stable → beta can be a downgrade. It depends only on the versions.

**What's new**: after an update, the app shows every `CHANGELOG.md` section
newer than the last version the user saw, up to the running one.

## Making a release

Beta build (the usual case). First write the `CHANGELOG.md` section, headed
with the exact version:

```markdown
## b290 — short title
```

Then:

```bash
./packaging/bump.sh small-feature    # b287 → b290; also: mini +1, medium +5, big +10, major +15
git push origin HEAD b290
```

Named types add up to at most +15 per release. A release they do not
measure takes a number instead, which is used as given: `bump.sh 25`.

`bump.sh` sets the version in `README.md` and the Arch `PKGBUILD`, commits
them together with the changelog as "Release b290: short title", and tags
that commit. It stops instead when the tag already exists (a published tag
is never moved), when the changelog has no section for the new version, or
when other changes are uncommitted and would end up in the release commit.

Stable release, cut from the newest beta build. The changelog section comes
first here too, headed with the full stable version:

```markdown
## v1.0.0+b290 — short title
```

```bash
./packaging/bump.sh stable 1.0.0     # commits README and the changelog, tags v1.0.0+b290
git push origin HEAD 'v1.0.0+b290'
```

It stops for the same reasons as a beta build: the tag exists, the changelog
has no section for it, or other changes are uncommitted.

Pushing the tag runs `.github/workflows/release.yml`. It first runs the whole
test suite on Linux, Windows and macOS (the CI workflow, called from the
release) and builds nothing if any of it fails. Then it embeds the tag in
the binary (`FLASHAGENT_VERSION`), publishes the assets to the rolling
`beta` or `stable` release, and rejects any tag that is neither `bN` nor
`vX.Y.Z+bN`. A stable tag also gets a permanent release of its own, named
after the tag, with that version's changelog section as its notes; the
rolling `stable` release is replaced by the next stable, this one stays.

### Choosing MAJOR.MINOR.PATCH for a stable release

- **PATCH**: bug fixes only.
- **MINOR**: new features. Existing configs, sessions and commands still work.
- **MAJOR**: something users rely on stops working as before, for example a
  config field is removed or a command or keybinding changes meaning.

Beta build numbers stay the same across stable releases: the next beta after
`v1.0.0+b290` is `b291` or higher.

## Local builds

A build without `FLASHAGENT_VERSION` reports the workspace version from
`Cargo.toml` (`0.1.0`). Builds from the source tree or `target/` never
self-update (see `is_dev_mode` in `crates/svc/src/updater.rs`).
