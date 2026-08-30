# Bedouin — plan draft for implementation agent

Declarative, single-binary environment manager. Replaces an Ansible + chezmoi
setup for bootstrapping and maintaining dev machines across Ubuntu, SUSE, and
macOS. One YAML config, versioned in GitHub, is the source of truth for
toolchains, packages, dotfiles, rc blocks, and PATH.

Status: approved. All decisions are final; the implementation agent can start from this document as-is.

## Why this exists (context for the agent)

- Ansible breaks with Python churn (3.10+, Galaxy); stock Python on Ubuntu is
  unreliable. The bootstrap tool must have **zero runtime dependencies**.
- Ansible steps don't reload the environment: installing Rust in one step
  leaves `cargo` off PATH for the next, forcing sequential playbooks. Bedouin
  must track install locations itself and never depend on the parent shell.
- chezmoi syncs *templates*; once rendered, edits to the rendered file can't
  flow back. Bedouin owns rendering and (v2) supports absorb-back.

## Stack

- **Rust**, Cargo workspace. GUI shell via **Tauri v2**.
- Workspace layout:
  - `bedouin-core` — schema types (serde), facts resolver, planner, executor,
    state store, reconciler. All logic lives here.
  - `bedouin-cli` — thin clap wrapper. **Pure static binary** (musl on Linux,
    universal on macOS). This is the only thing that runs on a fresh machine.
  - `bedouin-app` — Tauri v2 wrapper over core: config editor with schema
    validation, plan preview, drift dashboard, absorb review UI (v2).
- **Bootstrap invariant:** `bedouin-cli` must run on a freshly imaged OS with
  nothing installed — no Python, no webview. Tauri requires webkit2gtk on
  Linux, so the GUI is a companion app, never part of the bootstrap path.
- Template engine: minijinja.

## Schema v0 (`bedouin.yaml`)

```yaml
version: 0

vars:                      # user globals, referencable as {{ vars.x }}
  editor: nvim

targets:                   # per-environment overrides, first match wins
  - match: { os: macos }
    vars: { editor: nvim }

package_managers: [brew, apt, zypper, mise]

languages:
  - { name: rust, version: "1.80", installer: rustup }
  - { name: go,   version: "1.23" }

packages:
  - name: zellij
    from: cargo            # implies rust; auto-added with a warning if absent
    version: latest
    path: ["{{ home }}/.cargo/bin"]
    rc:
      - file: "{{ shell.rc_dir }}/70-zellij.zsh"
        content: |
          eval "$(zellij setup --generate-auto-start zsh)"
  - name: jq
    from: [brew, apt, zypper]   # fallback order; first available manager wins

files:                     # chezmoi-replacement: managed files & templates
  - src: templates/gitconfig.j2
    dest: ~/.gitconfig
```

Facts resolved by the engine, not declared: `os`, `arch`, `home`, `shell.name`,
`shell.rc_dir` (the drop-in dir), sudo availability, which managers exist.
The user asks "where is the rc dir" instead of writing conditionals.

Auto-resolution rules:
- Package `from: cargo` with no `rust` in `languages` → add rust implicitly, warn.
- Declared manager not installed → bedouin bootstraps it (brew, mise, rustup).

## Execution model

- `plan`: resolve facts → build DAG (managers → languages → packages → files →
  rc blocks → PATH) → diff against state → print terraform-style plan.
- `apply`: execute plan. Each step receives an explicit env: bin paths come
  from the state manifest (e.g. `~/.cargo/bin/cargo`), never from the parent
  shell's PATH. This is the fix for the Ansible reload problem.
- Idempotent; `--dry-run`; per-step sudo escalation (`sudo -n` probe, batch
  apt/zypper steps together).

## State file

`~/.local/state/bedouin/state.json`:
- `schema_version`, machine id, last apply
- per item: id, owner (`bedouin` | `preexisting`), version, install method,
  files written (path + hash), rc blocks (file, marker id, rendered hash),
  PATH entries
- rendered-output snapshots per template (enables 3-way absorb in v2)

Uninstall = remove from config → next apply removes only `owner: bedouin`
artifacts: package, its rc block, its PATH entries.

## Managed blocks & PATH

- rc content is written between sentinel markers:
  `# >>> bedouin: zellij >>>` … `# <<< bedouin: zellij <<<`
  Content between markers is owned by that config entry. Hash mismatch inside
  markers → `doctor` flags it; v2 `absorb` offers to lift the edit back into
  the config.
- PATH is never string-edited: bedouin renders one `00-bedouin-path.zsh` from
  the structured `path:` entries. Provenance and removal are automatic.

## CLI surface

v1: `init`, `plan`, `apply`, `doctor`, `sync` (git pull config + apply),
`add <mgr>:<pkg>[@ver]` (append to config + apply), `remove`.
v2: `absorb`, `reconcile --watch` (daemon mode).

## Milestones

1. **M0** — schema + facts resolver + `plan` output. No side effects yet.
2. **M1** — executor: managers, languages (rustup/mise), packages, rc blocks,
   PATH file, state store. `apply` works end to end on Ubuntu + macOS.
3. **M2** — `doctor`, drift detection, `remove`/uninstall, SUSE support.
4. **M3** — Tauri app: config editor, plan preview, drift dashboard.
5. **M4** — absorb / bidirectional sync (3-way: original render vs current
   file vs new render; marked-region edits map to their config entry).

## Non-goals (v1)

Windows; secrets management (reference external: 1Password CLI / age);
service orchestration; being a general config-management
tool. Bedouin is env setup, not Ansible.

## Icon brief (separate graphics agent)

Concept: **bayt al-sha'ar** — the black goat-hair Bedouin tent. Low, wide
silhouette; ridge line sagging between two poles; guy lines staked outward.
Monoline, single accent colour, light + dark variants.
Constraint found in exploration: the ridge-tent muddies below ~24px. Deliver a
pair: full mark as hero (docs, README), plus a reduced derivative (or simple
sharp-peak tent) for favicon/terminal sizes.

## Resolved calls

- **Binary name:** `bedouin`. No built-in short alias; ship shell completions,
  users who want `bdn` can alias it.
- **Config layout:** single `bedouin.yaml`, plus an optional `includes:` list
  of globs merged in declaration order — drop-in-directory style, matching the
  zsh subscript philosophy. Cheap to support in v0.
- **chezmoi:** coexist, don't integrate. Bedouin only touches files declared
  in `files:` (state-tracked), so migration is incremental — move a file into
  Bedouin's config, delete it from chezmoi. No day-one big bang.
- **Reconcile daemon:** in scope for v2. `bedouin reconcile --watch` plus
  `bedouin daemon install` generating launchd (macOS) / systemd user units
  (Linux).
