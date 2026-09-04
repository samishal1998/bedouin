# Changelog

Dates are release dates. Versions before 0.2.0 are omitted: they predate this
file and nothing depended on them.

## 0.15.1 — 2026-09-04

**Clearing a field in the browser wrote a null instead of removing it.**
`version:` with nothing after it parses as null, and null is a value — an
absent `version:` means "latest", a null one is a config saying something
nobody typed. `edit::unset_field` takes the key back out, and an emptied alias
is removed rather than set to nothing.

## 0.15.0 — 2026-09-04

**The web UI edits the config.** Add, change and remove entries in any
section, from the browser. Three things stand in the way of that being a
mistake, and none is optional.

**Writes exist only on a loopback bind.** `--hostname 0.0.0.0` still serves a
read-only view — the mutating routes are not registered at all, so they 404
rather than 403. An edit here decides what `bedouin apply` runs, and offering
that to a network that never proved who it was is not a thing to do by
default. Reach it through an ssh tunnel, which presents as loopback anyway.

**A header a form cannot set.** Binding to 127.0.0.1 is not a boundary against
a browser: any page you visit can POST to it, and a plain `<form>` goes
cross-origin with no preflight. A drive-by page could add a package with a
`script:` field and your next apply would run it under sudo. Every write must
carry `X-Bedouin: 1` — a form cannot set a header, and a `fetch` that sets one
becomes a preflight this server never answers. Origin is checked too, when the
browser sends it.

**Forms are seeded from the config's own text.** `/api/state` now ships the
raw value of every editable key beside the resolved one. This is the whole
game: `from: { macos: brew, default: apt }` RESOLVES to `apt` on Linux, and a
form seeded from that would write `apt` over the mapping and delete the macOS
arm — on a machine where nothing looks wrong afterwards. A key that no
single-line form can round-trip is left out and shown read-only rather than
flattened.

Every write goes through `run::write_verified`: back up, write, re-plan, and
put the original back if the result no longer loads. One lock spans the whole
read-modify-write, so two tabs cannot read the same text and write over one
another. The server hands the new snapshot back with the response rather than
making the page ask again and race the next edit.

The footer stopped claiming "this view reads" — it says which of the two
things an edit changes, and says so differently when the view is read-only.

## 0.14.0 — 2026-09-04

**Search.** `/` focuses it. Typing filters every section at once and the tab
counts become match counts, so "where is zellij" is answered without knowing
it is a package. If the section you are on has no matches and another does, it
moves you there. A row matches on everything it shows — its name, the detail
beside it, and every field in the pane that opens when you pick it. The band
answers the query too: non-matching threads fade rather than vanish, so where
a match sits in the whole plan stays visible.

**Marks.** One inline SVG sprite, no icon font and no request — this page is
embedded in a binary that runs on machines with no network. They replace the
tracked uppercase kind word, on the tent's own 24 grid and its 45-degree
diagonals. The action sigils stay text: `+ ~ -` in madder, ochre and olive
already read at a glance and belong to the monospace voice. The page also has
a real favicon at last, instead of the deliberate blank it has shipped since
0.10.

**Every config section is now editable.** `edit.rs` could only find an entry
by `name:`, so files, repos and links were invisible to every edit in the
module — no create, no update, no delete. They are keyed by `dest:` instead,
which is the only thing unique about them. `Section` grows an `id_field()`,
`add_entry` generalises `add_package`, and `remove_alias` closes the last gap.
Values are quoted only when leaving them bare would change their meaning:
`~/.config/nvim` is fine, but `1.80` is a float, `0644` is an int, `no` is
false and a bare `~` is null.

**Two things that were quietly wrong.** `verify_only_change` promised that an
edit changed nothing else and never checked — its body ended `let _ =
before_text;`. It counts now. And `expected_after`, the guard that a removal
touched exactly one entry, compared against a document with nothing removed
for any dest-keyed section.

**`write_config_verified` moved into core** as `run::write_verified`, so the
web UI can edit a config with the same net the CLI has: back up, write,
re-plan, and put the original back if the result no longer loads. It carries
the file's existing mode through the write now — `Host::write` always sets
one, and picking a number silently re-permissions a config that is in git.

The browser can search and read, not yet write; the endpoints are next.

## 0.13.0 — 2026-09-04

**`bedouin self upgrade`.** It checks for a newer release, shows what is out
of date, asks, and replaces the binaries in place. `--check` reports and exits
2 — the same "there is work pending" that `plan` and `doctor` use, so a timer
or a CI step can gate on it. `-y` skips the prompt. `bedouin self version`
says what is installed and needs no network at all.

**Not `bedouin upgrade`.** bedouin's entire subject is installing and
upgrading packages, so a bare `upgrade` would read as "bring my machine's
software up to date" — which is a thing bedouin deliberately does not do
(§7.2: `latest` means install if absent, never upgrade). The `self` namespace
is the difference between the tool and its subject.

**It upgrades the sidecar too, but only if you have one.** `bedouin ui` pins
`bedouin-ui` to an exact matching version, so upgrading bedouin and leaving a
stale sidecar behind just means the next `bedouin ui` re-downloads. Anyone who
has never run `bedouin ui` gets no web server pushed onto their machine by an
upgrade; it is still fetched on first use.

**How it finds the newest version.** `/releases/latest` redirects to
`/releases/tag/vX.Y.Z`, so the tag is in the URL curl lands on: one request,
no token, no JSON, and no 60-per-hour GitHub API rate limit shared with
everyone else behind the same NAT. A redirect that is not a release tag — a
sign-in wall, a moved repository — reads as no version rather than as one.

**Replacing a running binary.** Staged in a directory beside the target, then
renamed over it. The rename is atomic and within one filesystem, so it cannot
leave half a binary at the path, and creating that staging directory fails
immediately when the install directory needs root — which is a better error
than downloading nine megabytes first. It never silently escalates; it prints
the `sudo` command and stops.

Versions compare as numbers. As text `0.9.0` sorts above `0.12.1`, which would
have told everyone on 0.9.0 they were current and quietly stranded them there.

One download path, not two: `bedouin ui`'s fetch and this one are the same
code in `release.rs`, so the SHA256 verification cannot drift between them.

Upgrading *to* 0.13.0 still needs `install.sh` — 0.12.1 has no `self` command
to run. After that, `bedouin self upgrade` is the way.

## 0.12.1 — 2026-09-04

**Plan warnings are a banner again, not footer text.** 0.12.0 moved them into
the footer at 0.74rem, which is the wrong size and the wrong end of the page
for "this package was skipped because the machine has no brew". They sit
directly under the band now: a warning is a reason not to trust the verdict
above it, and it belongs next to what it undermines.

## 0.12.0 — 2026-09-04

**The web UI has a face of its own.** Madder is a dye before it is a hex
value, and it is the one bedouin weaving actually uses — so the palette is now
the rest of that dye pot: madder, ochre and olive, on goat-hair black or
undyed wool. The three dyes land where a developer already expects them, red
on remove and green on add: legibility outranks the conceit. Monospace is the
page's voice rather than its code font, because this is a machine describing
itself; the sans is demoted to annotation.

**The verdict moved to the top.** How many things this machine needs is the
only fact most visits are here for, and it used to be a sentence in the
footer. It is now the largest type on the page. When there is nothing to do it
says `camp is pitched`.

**The band.** One thread per item in the plan, dyed by what it does, running
edge to edge under the header — the rule between the verdict and the content
is the plan itself. Where the weave is dense with colour, the machine has work
to do; picking a thread selects its item. With nothing to do it falls back to
the bare warp, because a finished camp is a pattern too, not an empty box.
It is `aria-hidden` and out of the tab order on purpose: it is a pointer
shortcut to the list below, which carries the same items in the same order
with real names, and sixty tab stops in front of the content would be a worse
page for the people it claims to help.

**Two fixes behind the paint.** `action` in `/api/state` was Rust's `Debug`
output — `reinstall { from_method: "apt", to_method: "cargo" }` — where the
TUI has always said `reinstall via apt -> cargo`. `action_label` moved to
`bedouin-core`, so both front ends now spell an action one way. And a failed
`/api/state` showed `500 Internal Server Error` while throwing away the
message bedouin-ui had put in the body; the page shows the real reason now.

Empty sections say what would go there and the command that puts it there.

## 0.11.1 — 2026-09-04

**`bedouin ui --hostname`.** It still binds loopback by default — this serves
a machine's configuration, and that is not a thing to put on an interface by
accident — but `-H 0.0.0.0` is how you say you meant it.

Bind anywhere but loopback and it says so, once, at startup: there is no
authentication, so anyone who can reach the port can read the config, the
machine's facts and the *names* of the environment variables it reads. On a
rented box `0.0.0.0` is the internet, not a LAN.

Loopback is decided by asking the socket, not the string: `localhost` resolves
to `::1` and `127.0.0.2` is loopback too, and neither is spelled `127.0.0.1`.

## 0.11.0 — 2026-09-04

**The web UI has an interface.** Astro, built to a single inlined file and
embedded in the sidecar with `include_str!` — 16 KB, no runtime asset
lookup, because a binary that is *fetched into a directory of its own* cannot
expect to find files beside itself.

The same nine sections as the TUI — plan, packages, files, repos, links,
aliases, languages, doctor, env — with a list, a details pane, the mark's
palette in both light and dark, and `j`/`k` because the TUI has them. The
empty states get the tent.

**One endpoint, `/api/state`.** Nine would each re-probe the machine;
`run::plan` is the expensive call and it already produces everything the page
draws. A test asserts every key the page indexes by name still exists — a
rename there is a blank screen with nothing else to catch it.

The page is read-only for now: applying is still `bedouin apply`, and the
footer says so rather than showing a button that is not wired.

## 0.10.0 — 2026-09-04

**`bedouin ui` serves a web UI, from a separate binary.** The HTTP stack and
the built assets live in `bedouin-ui`, which is released beside `bedouin` and
never inside it. The bootstrap binary grew by **52 KB** — the logic to look
for the sidecar, fetch it, verify it and hand over. It costs that little
because the download is `curl` through the `Host`, exactly as brew, mise and
rustup are bootstrapped: no new dependency reaches the static musl binary.

Missing, it says what it will fetch, from where and into where, and asks.
Present, it `exec`s — replacing the process rather than spawning one, which is
what lets the server own the terminal you started it from. That is the answer
to the privilege problem the TUI dodged: **sudo prompts in your terminal**, and
no password crosses HTTP.

The sidecar's version must match; a plan rendered by a different core is a
plan for a different program. The tarball is checked against the release's
`SHA256SUMS` before anything is executed, and a release without published sums
is one this refuses to install.

Serving now: `/api/plan` and `/api/facts`, both real. The interface itself is
next.

## 0.9.0 — 2026-09-04

**`n` adds an entry.** Packages take a name, a manager and an optional
version; aliases take a name and a value. Both go through the same text
surgery as an edit, and both show the diff of what was written. Sections with
no way to add — files, repos, links, languages — say so and point at `e`
rather than offering a form that cannot deliver.

**The mark, in characters.** The tent from
`assets/icon/bedouin-mark-mono.svg` renders as ASCII: it fills the empty
states, and it goes up a row at a time while the first plan is computed. That
wait is real — planning probes the machine, resolves the config and reads
files — and it used to be spent on a blank terminal, because the plan ran
before the screen existed. Terminal first now, then plan.

## 0.8.0 — 2026-09-04

**The form edits every field, not just `version`.** A package offers `from`,
`version`, `only`, `needs`, `path` and `script`; a language offers
`installer`, `version` and `only` — every key `edit::set_field` can write.
`↑`/`↓` moves between them, `enter` commits the one you are on.

**And it edits what is written, not what was resolved.** This is the reason
the change is not simply "add more fields". `from: { macos: brew, default: apt }`
*resolves* to `apt` on Linux, so a form seeded from the resolved value would
have committed `from: apt` and silently deleted the macOS arm — quietly
breaking the other machine the config exists to serve. Fields are seeded from
the config text, so the condition is what you edit and what survives. There is
a test that commits a conditional back unchanged and asserts the macOS arm is
still there.

An entry written inline (`- { name: jq, from: apt }`) still cannot be
round-tripped by a one-line form, and still says so and points at `e`.

## 0.7.0 — 2026-09-04

**The TUI wears the mark's colours.** Madder — `#A82A24`, and the lift at
`#D4443C` — taken from `assets/icon/bedouin-mark-*.svg` and the names the
docs site already uses. Selection is sand on madder, the pairing the logo
itself uses. Set as RGB rather than one of the sixteen ANSI slots, because
those are whatever the reader's theme says they are, and a brand colour that
becomes someone's "bright red" is not a brand colour.

Additions stay green. That convention is older and louder than any brand, and
a palette where "will install" and "will remove" are the same hue is a palette
that lies. Everything else — changes, removals, drift, the chrome — sits in
the madder family.

**A details pane beside the list.** The selected item opened out: for a
package its managers, version, `needs`, `path`, aliases, completions and the
full text of each `rc:` block; for a plan step its id, action, matched arms
and payload; for drift what changed and what apply would do about it; for an
environment variable what happens if it stays unset. Below 90 columns the pane
is dropped rather than crushed, and the list takes the full width.

## 0.6.0 — 2026-09-04

**The TUI navigates the whole config, not just the plan.** Nine sections —
plan, packages, files, repos, links, aliases, languages, doctor, env — with
`tab` between them and a cursor kept per section, so moving away and back
returns you where you were.

**Editing, two ways.** `enter` opens a form on the fields the text surgery in
`edit.rs` can safely change; `e` opens `$EDITOR` at the item's line and
re-plans on return. Both exist because neither is sufficient: a form cannot
represent a conditional value like `from: { macos: brew, default: apt }`, and
it cannot edit an entry written inline as `- { name: jq, from: apt }` — the
majority spelling in a real config. It now says so and points at `e` rather
than surfacing a YAML parse error.

**`d` diffs three different things**, depending on what is selected: what
apply would write against what is on disk (a rendered template, or the block
inside an rc file), drift for a doctor row, and — after a form edit — the
before and after of the config itself.

Costs about 110 KB gzipped over the previous TUI: 2.10 MB against 1.99.

## 0.5.1 — 2026-09-04

**bedouin installs its own shell completion.** Nobody should have to declare
completions for the tool that is configuring their shell, so every plan now
carries a `completion/bedouin` item generated by re-invoking the running
binary. Works for bash, zsh and fish.

This also fixed the half that would have made it useless: the block that puts
the completions directory on `fpath` was only written when the config wrote
*other* shell files, so a minimal config got a completion in a directory
nothing read. That block is no longer conditional — which does mean bedouin
leaves one marked block in your rc file even for a config that declares
nothing else.

## 0.5.0 — 2026-09-03

**`bedouin tui`.** The plan on screen, `a` to apply it. Applying drops out of
the alternate screen and runs the ordinary apply — same output, same colours —
which is also what lets sudo prompt at all: `sudo -v` inherits stdin, and
inside a raw-mode screen that prompt is invisible and the run just hangs.

Behind a default-on cargo feature, so `--no-default-features` still builds the
minimal binary. Costs about 90 KB gzipped (1.99 MB against 1.90).

**`Line::Section` became `Line::Step` and `Line::StepEnd`.** *Breaking for
library consumers.* The step index used to be baked into a formatted string, so
a progress display had to parse `[3/47]` back out of it — and nothing at all was
emitted when a step ended, which made a step that succeeded, one that failed and
one still running the same silence. There is now exactly one `Step` before each
step and one `StepEnd` after it, with `ok` false when it failed.

**The plan is serializable.** `Plan`, `Item`, `Action`, `Payload`,
`apply::{Report, Failure}` and `doctor::{Report, Drift}` derive `Serialize`,
which is what a UI needs and what `Facts`, `Config`, `State` and `Artifact`
already had.

## 0.4.2 — 2026-09-01

Found by running the smoke test across twelve distros instead of two. Fedora,
Rocky, Alma, Debian 12/13 and Ubuntu 22.04 all worked first time; these are
what did not.

**`Distro::Opensuse` was unreachable on every real openSUSE machine.** No
shipping openSUSE reports `ID=opensuse` — Tumbleweed is `opensuse-tumbleweed`
and Leap is `opensuse-leap` — so an exact match on the ID left the variant
dead, and `only: opensuse` and `match: { distro: opensuse }` were silently
never true. The family arm kept working via `ID_LIKE`, which is why it hid.

**`bedouin facts` needed the config to resolve.** A config with a package that
has no arm for this machine took `facts` down with it, on exactly the
unsupported box where it is the first thing you would reach for. It now runs
on the loaded document and the probe alone, as `env` already did.

**One value had two spellings.** `str_enum!` derived `Serialize` with
`rename_all`, which renames the *variant*, so `ArchLinux` went out as
`arch_linux` while the arm a config writes is `arch`. The JSON is what any UI
consumes, so the two must agree; serialization now goes through `as_str`,
making the divergence unrepresentable rather than merely fixed.

**dnf gained tests.** It had install, remove and `needs_root` recipes and no
test of any kind — unit or container. Fedora is now in the CI smoke matrix,
and the rhel-like family is covered at the fake-host layer including Fedora's
missing `ID_LIKE` and the Rocky/Alma derivatives.

## 0.4.1 — 2026-09-01

**A failed state write no longer discards the apply report.** `apply` flushed
state with `?`, so an I/O error on `state.json` returned `Err` and took the
report with it — at the one moment that knowledge matters most, since the
record of what just ran is exactly what was lost. The flush failure is now the
report's `failure`, with `completed` intact and `not_attempted` naming the
rest. A step that succeeded stays in `completed` even when recording it
failed: saying otherwise would send the reader looking for work already done.

## 0.4.0 — 2026-08-31

**Toolchain bin directories reach your shell.** The generated PATH file was
built only from `path:` entries declared on packages, so nothing a *toolchain*
installed was ever on your PATH. mise would install neovim, node and go; the
run could see them, because the step environment adds those directories; your
shell could not. Every manager and language bin directory now goes into the
file — mise's shims, `~/.cargo/bin`, brew's prefix — and `go` contributes
`~/go/bin`, which is where `go install` puts things and is not where the
toolchain itself lives.

**`script:` installs a thing no manager packages.**

```yaml
- name: tailscale
  only: linux
  script: |
    curl -fsSL https://tailscale.com/install.sh | sh
```

Use it instead of `from:`, never with it. Presence is the binary being on
PATH, so it runs once. Bedouin cannot uninstall what a script installed, so it
does not record itself as the owner: dropping the entry forgets it rather than
pretending it was cleaned up.

## 0.3.1 — 2026-08-31

**`brew` bootstraps on a fresh Linux box.** Homebrew's installer stops at the
first missing prerequisite, and on a fresh machine they are all missing — git
especially, which it needs to clone itself with. Having them in `packages:` did
not help, because the manager phase runs before the package phase. The Linux
bootstrap now installs `build-essential procps curl file git` first, plus
`unzip`, which is not on Homebrew's list but which casks need.

## 0.3.0 — 2026-08-31

**Referencing a manager declares it.** `installer: rustup` or `from: cargo`
now bootstraps that manager whether or not it appears in `package_managers:`.
Forgetting to list it meant a fresh machine ran `rustup toolchain install`
against a rustup nothing had installed. Only bootstrappable managers are
implied: apt and zypper come with the distro, and a package asking for a
missing one already errors by name.

**A language uses its own installer by default.** `rust` resolves to rustup
rather than mise — it is how Rust is meant to arrive, and what
`rustup component add` and toolchain pinning expect. mise remains the default
for languages that ship no first-party installer.

**`apply --skip`.** One failing step no longer strands the rest of a run:

```sh
bedouin apply --skip 1password-cli,caddy
bedouin apply --skip package/jq      # the id a failure prints works too
```

Skipped steps are named in the report, never silently dropped, and a failure
message now suggests the flag.

**Readable output.** A heading per step, dimmed command output, and coloured
sigils in the plan. Colour is decided once from whether stdout is a terminal,
`NO_COLOR`, and `TERM`, so pipes, CI logs and library runs get plain text.

## 0.2.1 — 2026-08-31

**`bedouin env` reports what the config actually reads.** Three defects, all
in one scanner:

- It searched every string for `env.`, so the rc file name `40-direnv.zsh`
  reported a variable called `zsh`. Matching is now confined to `{{ … }}` and
  `{% … %}`, and `{% raw %}` bodies and `{# … #}` comments are skipped.
- It never opened the files a `files:` entry points at, so
  `{{ env.GIT_USER_NAME }}` inside a template was invisible. Because the same
  function decides what the plan artifact freezes, **a plan reviewed with one
  value could apply with another, silently.** Templates are now scanned.
- Guardedness was a substring test for `default`, which called
  `{{ env.CONFIG_DIR ~ '/defaults.toml' }}` safe. It now requires a real
  `| default` filter whose fallback resolves, and accepts `| d(…)` and
  `is defined`.

`env['NAME']` is recognised as the same read as `env.NAME`; a `}}` inside a
string literal no longer truncates the expression; a `src:` that escapes the
config root is no longer read; and `apply -f` no longer plans against the live
config first, which had defeated the frozen environment in exactly the case it
exists for.

## 0.2.0 — 2026-08-31

`bedouin env` and `.env.bedouin`; `bedouin alias` and `bedouin completions`;
`repos:` for configuration that lives in a git repository, and `links:` for
symlinks bedouin owns; shell frameworks (`framework: oh-my-zsh`, with theme
and plugins written above the line that reads them).
