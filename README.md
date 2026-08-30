<div align="center">

<img src="assets/icon/bedouin-hero-light.svg#gh-light-mode-only" width="360" alt="Bedouin">
<img src="assets/icon/bedouin-hero-dark.svg#gh-dark-mode-only" width="360" alt="Bedouin">

**One config. Every machine.**

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
shell: zsh                    # the shell you are configuring -- on a fresh
                              # box that is usually not the one you are running

vars:
  editor: nvim

targets:                      # named conditions, for axes no enum can know
  - name: noble
    match: { distro: ubuntu, distro_version: ">=24.04" }

aliases:
  ll: ls -alh

packages:
  - name: fd
    from: { macos: brew, default: [apt, zypper] }   # a mapping means branches

  - name: xclip
    from: apt
    only: linux                                      # membership, not value

  - name: zellij
    from: cargo
    needs: [build-essential]
    path: ["{{ home }}/.cargo/bin"]
    aliases: { z: zellij }
    completions:
      generate: ["zellij", "setup", "--dump-completion", "{{ shell.name }}"]

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
| `bedouin apply` | make it so |
| `bedouin doctor` | report managed content edited by hand (exit 2 = drift) |
| `bedouin absorb` | lift those edits back into the config |
| `bedouin add cargo:zellij@0.40.1` | add a package, then apply |
| `bedouin remove zellij` | drop it, then undo it on this machine |
| `bedouin sync` | pull the config repo, then apply what changed |
| `bedouin reconcile --watch` | keep the machine matching, unattended |
| `bedouin daemon install` | write the systemd/launchd unit that runs it |

`plan -o plan.json` then `apply -f plan.json` applies exactly the plan you
reviewed — including the environment it read, so a plan reviewed in one
terminal cannot mean something else in another.

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

`docs/superpowers/specs/2026-08-30-bedouin-m0-m1-design.md` is the design, and
it is kept honest: §14a, §14b, §16 and §17 record every place the shipped code
departs from the original plan, and why.
