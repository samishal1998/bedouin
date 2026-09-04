<div align="center">

<img src="assets/icon/bedouin-hero-light.svg#gh-light-mode-only" width="360" alt="Bedouin">
<img src="assets/icon/bedouin-hero-dark.svg#gh-dark-mode-only" width="360" alt="Bedouin">

**One config. Every machine.**

[Docs](https://samishal1998.github.io/bedouin/) ·
[Install](https://samishal1998.github.io/bedouin/guides/install/) ·
[Why](https://samishal1998.github.io/bedouin/guides/why/)

[![ci](https://github.com/samishal1998/bedouin/actions/workflows/ci.yml/badge.svg)](https://github.com/samishal1998/bedouin/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/samishal1998/bedouin?color=A82A24)](https://github.com/samishal1998/bedouin/releases)

</div>

Bedouin is a declarative environment manager: a single static binary that takes
a freshly imaged machine and makes it yours, from one `bedouin.yaml` you keep
in git.

It replaces an Ansible + chezmoi setup, and it exists because both of them
break in the same two places. Ansible needs a working Python, which is the one
thing a fresh machine cannot promise — so Bedouin has **zero runtime
dependencies**. And Ansible's steps do not reload the environment, so
installing Rust in one step leaves `cargo` off `PATH` for the next — so Bedouin
**tracks where it put things** and builds each step's environment itself,
rather than inheriting your shell's.

```sh
curl -fsSL https://samishal1998.github.io/bedouin/install.sh | sh
```

```console
$ bedouin plan
Bedouin will make the following changes:

  + manager   brew                          not installed
  + language  rust        1.80              rustup
  ~ package   zellij      0.39.2 -> 0.40.1  cargo
  + file      ~/.gitconfig                  from templates/gitconfig.j2
  + rc        ~/.zshrc.d/70-zellij.zsh      owned by zellij

Plan: 4 to add, 1 to change, 0 to remove.
```

## The config

```yaml
version: 0

shell:
  name: zsh                   # declared, not detected -- on a fresh box that is
  framework: oh-my-zsh        # usually not the shell you are running
  theme: agnoster
  plugins: [git, docker]

vars:
  editor: nvim
  pm: { macos: brew, debian-like: apt }   # say it once, use it everywhere

targets:                      # named conditions, for axes no enum can know
  - name: noble
    match: { distro: ubuntu, distro_version: ">=24.04" }

aliases:
  ll: { linux: ls -alF, default: ls -la }  # alias values take arms too

packages:
  - name: fd
    from: "{{ vars.pm }}"

  - name: xclip
    from: apt
    only: linux                            # membership, not value

  - name: build-essential
    from: apt
    only: linux

  - name: zellij
    from: cargo
    needs: [build-essential]
    path: ["{{ home }}/.cargo/bin"]
    aliases: { z: zellij }
    completions:
      generate: ["zellij", "setup", "--dump-completion", "{{ shell.name }}"]

repos:                        # config that lives in a git repository
  - url: https://github.com/gpakosz/.tmux
    dest: "{{ home }}/.tmux"

links:                        # symlinks bedouin owns
  - src: "{{ home }}/.tmux/.tmux.conf"
    dest: "{{ home }}/.tmux.conf"

files:
  - src: templates/gitconfig.j2
    dest: ~/.gitconfig
```

**A YAML mapping where a value is expected means branches.** There is no
`when:`, no `select:`, no `matcher:` — the shape carries the meaning, so the
common case stays one line. Arm names come from a closed vocabulary, so a typo
is an error rather than a branch that silently never matches on a machine you
are not sitting at. The more specific arm wins regardless of the order you
wrote them in.

## Commands

| | |
|---|---|
| `bedouin init` | write a starter config |
| `bedouin plan` | show what would change (exit 2 = changes pending) |
| `bedouin tui` | browse the config and the plan, edit, diff, apply |
| `bedouin ui` | the same in a browser, from a sidecar binary (loopback only unless you pass `--hostname`) |
| `bedouin apply` | make it so |
| `bedouin apply --skip jq` | ...without one step that this machine cannot do yet |
| `bedouin doctor` | report managed content edited by hand (exit 2 = drift) |
| `bedouin env` | which environment variables the config reads, and whether they are set |
| `bedouin absorb` | lift those edits back into the config |
| `bedouin add cargo:zellij@0.40.1` | add a package, then apply |
| `bedouin remove zellij` | drop it, then undo it on this machine |
| `bedouin alias gs='git status'` | set an alias without opening the config |
| `bedouin completions gh -- gh completion -s zsh` | same, for a completion generator |
| `bedouin sync` | pull the config repo, then apply what changed |
| `bedouin reconcile --watch` | keep the machine matching, unattended |
| `bedouin daemon install` | write the systemd/launchd unit that runs it |
| `bedouin self upgrade` | check for a newer bedouin, and the sidecar, then install it |
| `bedouin self version` | what is installed here — works with no network |

`plan -o plan.json` then `apply -f plan.json` applies exactly the plan you
reviewed — including the environment it read, so a plan reviewed in one
terminal cannot mean something else in another.

## Beyond packages

**Your shell, not just your tools.** `framework: oh-my-zsh` installs it if it is
absent and writes the theme and plugin list into a block *above* the line that
reads them — appended at the end, as every other block is, it would be a silent
no-op.

**Config that lives in a repository.** `repos:` clones it; `links:` puts a
subdirectory of it where the tool expects to find it. That is how oh-my-tmux
installs, and how a neovim config inside a dotfiles repo gets to `~/.config/nvim`
while keeping its history.

**Saying it once.** `vars` values take arms like anything else, so
`pm: { macos: brew, debian-like: apt }` written once serves every
`from: "{{ vars.pm }}"` below it.

**Things no manager packages.** `script:` runs an installer that is not a
package — tailscale's registers a repository and starts a daemon, which no
`from:` can express. Bedouin runs it once and, because it cannot undo it,
never claims to own the result.

**It declares what it needs.** `installer: rustup` or `from: cargo` is enough:
the manager is bootstrapped whether or not `package_managers:` lists it. What
a toolchain installs lands on your PATH too, not just the run's.

## What it promises

- **It tells you before it acts.** `plan` is a faithful prediction of `apply`,
  and where it cannot be, it says so rather than guessing.
- **It only removes what it installed.** Anything already on the machine when
  Bedouin first ran is adopted, never owned, and survives being dropped from
  the config.
- **It does not eat your files.** A managed file that displaces one of yours is
  backed up first and given back on removal. Bedouin owns *blocks* inside your
  rc files, never the files.
- **A failure stops the run** and names what it did not attempt. Re-running
  resumes, because `plan` re-diffs.

## Building

```console
cargo build --release --target x86_64-unknown-linux-musl   # static, 4.6M
```

The binary must run on a machine with nothing installed, so the bootstrap path
never links a webview and never needs a runtime. `bedouin-core` holds all the
logic behind one `Host` trait; `bedouin-cli` is a thin clap wrapper.

## Design

Full documentation: **https://samishal1998.github.io/bedouin/**

`docs/superpowers/specs/2026-08-30-bedouin-m0-m1-design.md` is the design, and
it is kept honest: §14a, §14b, §16 and §17 record every place the shipped code
departs from the original plan, and why.

## Status

Everything here works and is tested on Ubuntu, SUSE, Fedora and macOS: 256 tests, plus
a real `apply` against real package managers inside containers on every push.
[CHANGELOG.md](CHANGELOG.md) has what landed when.
The Tauri companion app is the one thing not built yet — the bootstrap binary
must never need a webview, so it was always a separate, later concern.
