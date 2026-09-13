# `bedouin install` — packages from GitHub Releases

*Design, 2026-09-13. Status: awaiting review.*

## The problem

A large amount of good software is published only as a GitHub release: a
tarball per platform, a tag, and a README that says `curl ... | sh`. None of it
is in apt. Installing it means running somebody's script, and the machine then
has a binary no config knows about — the same gap `pickup` exists to close,
arriving through a different door.

`eget` and `houseabsolute/ubi` solve the fetching half. Neither makes the
result declarative, which is the half bedouin already has.

## What it is

    bedouin install sharkdp/fd              # newest stable release
    bedouin install sharkdp/fd@v10.5.0      # that tag
    bedouin install helix-editor/helix@prerelease
    bedouin install foo/bar@/^tui-/         # newest tag matching a regex

and the same thing declaratively:

    packages:
      - name: sharkdp/fd
        from: github
        version: v10.5.0        # optional; absent means "newest at first install"

`bedouin add github:sharkdp/fd` also works: `add` already splits a spec on the
first colon, and `sharkdp/fd` survives that intact.

## Decisions taken before design

These were settled with the user and are not re-opened below.

- **Both doors.** A verb for the one-off, `from: github` for the config. One
  engine behind them.
- **`bedouin-installer` is a workspace member over `bedouin-core`.** One
  implementation of the matcher, which is the subtlest code here. The
  standalone binary carries core and will be megabytes, not kilobytes. This is
  the same trade `bedouin-ui` already makes.
- **Checksums: verify when present, refuse on mismatch, say so plainly when
  absent.** fd and helix publish none; refusing outright would block much of
  the ecosystem.
- **Identity is `org/repo`, version is the tag.** State key
  `package/sharkdp/fd`. Slashes in item ids already work — repos use
  `repo//root/.config/nvim`.

## Constraints inherited from the codebase

**No HTTP stack is linked in.** `release.rs` says it outright: everything goes
through `curl`, `tar` and `shasum` on the `Host`, because the binary has to run
on a bare machine. This feature does the same, including archive extraction.
`serde_json` is already a workspace dependency and parses the API responses.

**`plan` runs no commands.** That property is why it is safe on the hot paths —
every web API read, every TUI refresh, every reconcile tick. Nothing here may
put a network call in `plan`. Presence is decided from state plus the binary on
disk; resolution happens in `apply` and in the `install` verb.

## 1. Resolution

`self upgrade` deliberately avoids the GitHub API by following the
`/releases/latest` redirect, which costs no rate limit. That trick cannot
enumerate assets, and the regex mode needs the release *list*, so this uses the
API:

    GET /repos/{org}/{repo}/releases/latest      # @latest (default)
    GET /repos/{org}/{repo}/releases/tags/{tag}  # @v1.2.3
    GET /repos/{org}/{repo}/releases?per_page=30 # @prerelease, @/regex/

Anonymous requests are limited to 60/hour per IP. `gh auth token` is used when
`gh` is on PATH — the same reasoning as `gitcmd.rs`, which appends
`gh auth git-credential` rather than replacing the user's helpers. When the
limit is reached, the error says so and names the fix:

    bedouin: GitHub is rate-limiting anonymous requests (60/hour, 0 left).
      `gh auth login` raises this to 5000/hour, and bedouin will use that token.

**Why `@/regex/` cannot be a filter over `latest`:** a repo publishing
`tui-0.1.0` and `cli-0.1.0` has one `/releases/latest`, and it is whichever was
cut last. Selecting a product means listing releases and taking the newest tag
that matches. The regex is matched against the tag, anchored by the user.

## 2. Asset matching

The core of the feature, and the place a wrong answer is worst: `helix` ships
`helix-25.07.1-source.tar.xz` in the same release as its binaries.

**Filter first. The filter is where safety lives.**

Rejected outright, whatever else they score:

| Class | Examples |
|---|---|
| Signatures and sums | `.sha256`, `.sha256sum`, `.sig`, `.asc`, `.pem`, `.sbom` |
| Metadata | `.txt`, `.json`, `.yaml`, `.zsync` |
| Source | any token `source`, `src`, `sources` |
| Debug output | `debug`, `symbols`, `dbgsym` |
| Another manager's job | `.deb`, `.rpm`, `.apk`, `.msi`, `.pkg` |
| The wrong OS | on Linux: `darwin`, `macos`, `apple`, `windows`, `.exe`, `.dmg` |

**Then score what survives**, against `facts`:

    +100  arch token matches        x86_64|amd64|x64  /  aarch64|arm64
    -1000 a *different* arch token is present         (effectively a rejection)
    +50   OS token present and correct
    +30   `musl`   on Linux   (bedouin is itself musl; static is the safer default)
    +10   `gnu`    on Linux
    -1000 `gnu`    on DistroLike::Alpine               (there is no glibc there)
    +5    a known archive extension                    .tar.gz .tgz .tar.xz .tar.bz2 .zip
    +5    `.AppImage` on Linux
    +1    the asset name contains the repo name

**A tie at the top, or a best score of zero, is a refusal** — printing every
asset in the release and the flag to name one explicitly. Never a guess. This
is the `plan.rs` `Owner::Preexisting` guard of this feature: the arm whose job
is to stop the plausible-but-wrong thing from happening quietly.

## 3. Verification

Two shapes exist in the wild and both are handled:

- a sidecar per asset — `ripgrep-15.2.0-x86_64-unknown-linux-musl.tar.gz.sha256`
- one file for the release — `checksums.txt`, `SHA256SUMS`, `*.sha256sum`,
  containing a line per asset

Verified through `shasum`/`sha256sum` on the `Host`, as `release.rs` already
does. A mismatch is a hard refusal and the download is discarded. **No
checksums at all is a printed sentence, not a failure** — it is the common case
and blocking it would block fd and helix.

## 4. Extraction and placement

By extension, shelling out:

| | |
|---|---|
| `.tar.gz`, `.tgz` | `tar -xzf` |
| `.tar.xz` | `tar -xJf` |
| `.tar.bz2` | `tar -xjf` |
| `.zip` | `unzip -o` |
| `.AppImage` | no extraction; `chmod +x` and place |
| no extension | assume a bare binary; `chmod +x` and place |

`unzip` is not in the ssh bootstrap's tool list and is absent from several base
images. A `.zip` asset on a machine without it is refused with a sentence
naming `unzip`, in the same pre-flight style as the `tar` check in
`gitcmd::subdir_export` — before anything is written.

**Finding the binary inside the archive**, in order: the manifest's `bin:`; else
the single executable file in the extracted tree; else a file named after the
repo; else a refusal listing what was found.

**Placement** is `~/.local/bin/<name>`, mode 0755, written beside and `mv`d into
place — the same atomic swap `release::install_over` already performs, so an
interrupted install never leaves a half-written binary on PATH.

## 5. The manifest

Optional, and autodetect must work without it — a manifest may never be the
thing that makes a tool installable, only the thing that makes it *better*.

Two locations, checked in this order:

1. `bedouin.yaml` / `bedouin.json` in the repository, read from
   `raw.githubusercontent.com` (no API call, no rate limit)
2. `manifest.yaml` / `manifest.json` as a release asset — already in the asset
   list, so it costs nothing extra to notice

```yaml
# bedouin.yaml, at the repository root
name: fd                     # what to call the installed binary
description: A fast find     # shown by `bedouin install --search` and the web UI
homepage: https://…
bin: fd                      # path inside the archive; supports {version}, {target}
strip_components: 1          # for archives with a top-level directory
assets:                      # overrides autodetect entirely, per target
  x86_64-unknown-linux-musl: fd-v{version}-x86_64-unknown-linux-musl.tar.gz
  aarch64-apple-darwin:      fd-v{version}-aarch64-apple-darwin.tar.gz
checksums: SHA256SUMS        # name the file if it is not one bedouin recognises
completions:                 # reuse the existing completions machinery
  bash: "{bin} --gen-completions bash"
```

Every field is optional. `assets:` is the escape hatch for a project whose
naming the matcher cannot score — it turns "bedouin guesses" into "the
maintainer said".

## 6. `bedouin install generate`

Emits a POSIX `sh` script for **one resolved release**:

    bedouin install generate sharkdp/fd@v10.5.0 > install-fd.sh

The script contains a `case "$(uname -sm)"` table of direct asset URLs and
their checksums, resolved at generate time. It does not call the GitHub API, so
it has no rate limit and no token; it is version-pinned, and regenerating is
how you publish a new one.

**This is deliberately not a general asset matcher in shell.** That would be a
second implementation of the subtlest code in the feature, in the language
least able to express it, and the two would drift. Bedouin's own `install.sh`
is already exactly this shape, which is the argument for it.

## 7. Integration

- **plan** — `from: github` resolves like any package. Presence is
  state plus the binary on disk; no network. A `version:` differing from state
  fires the existing `Upgrade` arm. `Manager::Github.pins_versions()` is true.
- **apply** — `recipe::installed(Github, …)` is the binary existing at its
  placement path. The resolve/download/verify/extract sequence is the install.
- **`needs_root`** is false: `~/.local/bin` is the user's.
- **`bin_dirs`** needs a `github` arm returning `~/.local/bin`. It does *not*
  come for free: `bin_dirs` is keyed by string with a `_ => vec![]` default, so
  a missing arm means an empty answer, the directory never reaches `step_env`,
  and everything installed this way is invisible to later steps. That exact bug
  shipped once for npm and was caught in a container, not by a test.
- **pickup** — `list_manual(Github)` is `None`, and this is not a gap that can
  be closed: GitHub is not a manager with an installed-set to query. A binary
  in `~/.local/bin` could have come from anywhere. Stated in the docs rather
  than guessed at.

## 8. Order of work

Each is a release, and each is useful on its own.

1. **`bedouin install org/repo[@sel]`** — resolve, match, verify, extract,
   place, record. Autodetect only. The whole value of the feature is here.
2. **The manifest** — both locations, overriding autodetect.
3. **`from: github`** — plan/apply/state/docs integration.
4. **`generate`**.
5. **`bedouin-installer`** — the standalone binary.

## 9. Deliberately not in scope

- **Windows.** bedouin is Linux and macOS; `.exe` and `.msi` are filtered out
  rather than handled.
- **GitLab, Codeberg, forgejo.** The resolution layer is written so a second
  host is a new module rather than a rewrite, but nothing is built for one now.
- **Building from source** when no binary asset matches. That is a different
  feature with a different failure mode.
- **`pickup` for GitHub-installed tools** — see §7.

## 10. Risks

- **Rate limits are the most likely support question.** Mitigated by `gh auth
  token` and a message that names the fix; not eliminated.
- **The matcher will be wrong for some repository.** Mitigated by refusing
  rather than guessing, by `--asset` to name one, and by the manifest. The
  refusal path matters more than the scoring.
- **Checksums are absent more often than one would like**, so a compromised
  release is detectable only where the maintainer helped. Stated rather than
  papered over.
