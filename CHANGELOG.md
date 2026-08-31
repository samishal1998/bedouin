# Changelog

Dates are release dates. Versions before 0.2.0 are omitted: they predate this
file and nothing depended on them.

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
