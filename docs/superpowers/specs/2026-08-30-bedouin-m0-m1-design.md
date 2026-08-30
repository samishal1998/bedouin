# Bedouin M0+M1 — design

Status: draft for review
Scope: M0 (schema, facts, `plan`) and M1 (executor, state, `apply`)
Supersedes nothing. Extends `bedouin-handoff.md`, which remains the source of
truth for anything this document does not contradict.

## 1. Scope

M0 and M1 ship one binary that takes a machine from freshly imaged to fully
configured, and reports honestly what it would do before it does it.

In scope:

- `bedouin.yaml` schema v0, including `includes:` and conditional values.
- Facts resolver.
- Config loading, arm selection, template rendering.
- `plan`: build the DAG, diff against state, print a terraform-style plan.
- `apply`: execute the plan. Package managers, languages, packages, managed
  files, rc blocks, PATH.
- State store at `~/.local/state/bedouin/state.json`.
- Ubuntu and macOS. SUSE parses and plans correctly but is not a tested
  execution target until M2.

Out of scope for M0+M1, in the order they are expected to arrive:

- `doctor`, drift detection, `remove` (M2).
- Tauri app (M3).
- `absorb`, `reconcile --watch` (M4).
- **All execution of user-supplied code during `plan`** — `sources:`,
  `fromScript`, matcher `script:`. See §6.5 for why this is a correctness
  decision rather than a scheduling one.

## 2. Crate layout

    bedouin-core/     schema, facts, loader, resolver, planner, executor,
                      state store. All logic. No I/O except through Host.
    bedouin-cli/      clap wrapper. Static binary. The only thing that runs
                      on a fresh machine.

`bedouin-app` (Tauri) is not created until M3. Creating it now would put a
webkit2gtk-dependent crate in the workspace that the bootstrap path must
never need.

Build target for release: `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl` on Linux, universal binary on macOS. The musl
targets are not installed on the current dev machine; `rustup target add` is a
prerequisite for the first M1 release build.

## 3. Facts

Facts are resolved by the engine, never declared. The user asks "where is the
rc dir" instead of writing conditionals about it.

| Fact | Type | Notes |
|---|---|---|
| `os` | `macos` \| `linux` | |
| `distro` | `ubuntu` \| `debian` \| `fedora` \| `opensuse` \| `arch` \| `other` | from `/etc/os-release` `ID`; `macos` on macOS |
| `distro_like` | `debian` \| `rhel` \| `suse` \| `arch` \| `none` | from `ID_LIKE`, falling back to `ID` |
| `distro_version` | string | `VERSION_ID`, e.g. `24.04`; macOS product version |
| `arch` | `x86_64` \| `arm64` | |
| `home` | path | |
| `user` | string | |
| `hostname` | string | short name, not FQDN |
| `shell.name` | `zsh` \| `bash` \| `fish` | see §3.1 |
| `shell.rc_file` | path | `~/.zshrc`, `~/.bashrc`, … |
| `shell.rc_dir` | path | drop-in directory; see §3.1 |
| `sudo` | `none` \| `passwordless` \| `password` | `sudo -n true` probe |
| `env` | map<string,string> | process environment |
| `managers` | set | which of brew/apt/zypper/dnf/mise/cargo/rustup exist |

`distro: other` is deliberate. An unknown distro must be representable, or
Bedouin cannot run at all on a machine it has not been taught about.

### 3.1 Shell and rc_dir

`shell.name` comes from `$SHELL`, falling back to the login shell in
`getent passwd $USER` (Linux) or `dscl . -read /Users/$USER UserShell`
(macOS). `$SHELL` is preferred because it reflects what the user actually
uses; the passwd lookup exists because `$SHELL` is absent under some CI and
container invocations.

`shell.rc_dir` is the drop-in directory: `~/.zshrc.d`, `~/.bashrc.d`,
`~/.config/fish/conf.d`. Bedouin creates it if absent and, for zsh and bash,
ensures the rc file sources it via a managed block (§9). Fish sources
`conf.d` natively and needs no block.

This is the one place M0's "no side effects" rule bends: `plan` reports
`+ create ~/.zshrc.d` and `+ managed block in ~/.zshrc` as plan items rather
than doing anything. Creation happens in `apply`.

### 3.2 Facts are not matchable if Bedouin installs them

`managers` and `shell` are **excluded from the match vocabulary** (§6.2).
Matching on them is circular: Bedouin installs package managers and shells, so
an arm keyed on one is chosen from the pre-Bedouin state of the machine and is
wrong on exactly the fresh-box case the tool exists for. They remain readable
in templates, where the same hazard exists but is visible at the point of use.

## 4. Config loading pipeline

Six ordered stages. Each is a pure function except stage 1.

    1. read      bedouin.yaml + includes: globs, in declaration order
    2. merge     concatenate list sections, last-wins on scalar collisions
    3. collect   pass one: read every `targets:` entry, build the name set
    4. parse     pass two: deserialize with the name set in scope
    5. select    Value<T> -> T against resolved facts. Depth 1. Total.
    6. render    minijinja over the surviving scalars
    -> Config, fully concrete, containing no conditionals

Stage 2 before stage 3 is load-bearing: a target declared in
`conf.d/10-targets.yaml` must be in scope for `conf.d/20-packages.yaml`.
Collecting names per-file would reject configs that are correct as a whole.

Stage 5 before stage 6 means losing arms are never rendered. A
`{{ home }}/.cargo/bin` inside a `ubuntu:` arm is not evaluated on a Mac, so a
template that is only valid on one platform costs nothing on the others.

Stages 5 and 6 both run before the planner. **The planner and the state file
never see a conditional value.** This is what keeps conditionals from
infecting the diff, the DAG, and the state schema.

## 5. Schema v0

```yaml
version: 0                    # schema version, not a bedouin version

includes:                     # optional; globs, merged in declaration order
  - conf.d/*.yaml

vars:
  editor: nvim

targets:                      # named conditions; see §6
  - name: work
    match: { env: { BEDOUIN_PROFILE: work } }
    vars: { editor: vim }     # optional: targets may still set vars

package_managers: [brew, apt, zypper, mise]

languages:
  - name: rust
    version: "1.80"
    installer: rustup

packages:
  - name: zellij
    from: cargo               # implies rust; auto-added with a warning
    version: latest
    path: ["{{ home }}/.cargo/bin"]
    rc:
      - file: "{{ shell.rc_dir }}/70-zellij.zsh"
        content: |
          eval "$(zellij setup --generate-auto-start zsh)"
  - name: jq
    from: [brew, apt, zypper] # fallback order; first available manager wins

files:
  - src: templates/gitconfig.j2
    dest: ~/.gitconfig
```

Unknown keys are rejected (`deny_unknown_fields`) with a did-you-mean. A
config tool that silently ignores a misspelled key is a config tool that
silently does the wrong thing.

## 6. Evaluatable values

The rule is one sentence: **a YAML mapping where a value is expected means
branches; anything else is the literal value.**

```yaml
packages:
  - name: fd
    from:
      macos: brew
      default: [apt, zypper]     # a list is still just a value
    version: latest              # a scalar is still just a value
```

There is no `Value` keyword, no `select:`, no `when:`, no `matcher:`. The
shape of the YAML carries the meaning, so the common case stays one line and
the conditional case stays four.

### 6.1 Arm names

Arm keys are drawn from a **closed vocabulary**: built-in names, plus the
names declared under `targets:`. A key outside that set is a parse error with
a did-you-mean.

This is the property the whole design is bought for: the vocabulary does not
depend on the machine, so **a config is valid or invalid identically
everywhere** — only which arm *wins* varies. A typo cannot become a branch
that silently never matches on any machine you happen not to be sitting at.

Built-in names are the static cross product of the facts enums:

- os: `macos`, `linux`
- distro: `ubuntu`, `debian`, `fedora`, `opensuse`, `arch`
- distro_like: `debian-like`, `rhel-like`, `suse-like`
- arch: `x86_64`, `arm64`
- pairs: `{os}-{arch}` and `{distro}-{arch}` — `macos-arm64`,
  `ubuntu-x86_64`, …

Not included, per §3.2: shell names, manager names.

### 6.2 Declared targets

`targets:` is the escape hatch, and the answer to every axis a compiled-in
enum cannot know. It carries the only match language in the file.

```yaml
targets:
  - name: noble
    match: { distro: ubuntu, distro_version: ">=24.04" }
  - name: work
    match: { env: { BEDOUIN_PROFILE: work } }
  - name: laptop
    match: { hostname: khaymah }

packages:
  - name: neovim
    from: { noble: apt, default: cargo }    # apt's nvim is stale before 24.04
    version: { work: "0.9.5", default: latest }
```

`match:` keys: `os`, `distro`, `distro_like`, `distro_version`, `arch`,
`hostname`, `env`. Scalar values match exactly; lists match any; strings
beginning with an operator (`>=`, `>`, `<=`, `<`) compare as versions.
An empty `match: {}` matches everything and is a parse error — write
`default:` instead.

`distro_version` is why the vocabulary cannot be closed-and-only-closed: the
most common reason a bootstrap config needs a conditional at all is "Ubuntu
22.04 ships an unusably old X", and versions are not enumerable. Declaring a
target for it keeps arm names closed while leaving the *axes* open, and
requires no Bedouin release to address a new machine class.

A target name that collides with a built-in name is a parse error.

### 6.3 Selection: most specific wins, order-independent

An arm's **specificity** is the number of facts it pins: built-in `macos` is
1, `macos-arm64` is 2, a declared target is the number of keys in its `match`
(an `env:` map counts each key). The most specific active arm wins. Written
order is irrelevant — the map reads like a set because it *is* one.

This is not a preference. Written-order selection is a live footgun:

```yaml
version:
  macos: "1.80"
  macos-arm64: nightly     # under written-order, unreachable on every machine
  default: stable
```

On an Apple Silicon Mac both arms are active. Under first-match-wins the
`macos-arm64` arm — which exists solely for that machine — can never fire, on
any machine, with no warning, and reordering two lines in a merge silently
changes the meaning of the config. Under most-specific-wins it resolves to
`nightly`, and the file can be sorted by a formatter without changing
behavior.

**Ambiguous ties are a parse error.** Two arms of equal specificity whose
fact sets can co-occur (`macos` and `arm64`, both specificity 1, both true on
one machine) are rejected at parse time, naming both arms and suggesting the
conjunction `macos-arm64`. Arms that cannot co-occur (`macos` and `ubuntu`)
are fine. Co-occurrence is decidable over a closed vocabulary, which is the
second thing the closed vocabulary buys.

### 6.4 `default` is optional, and its absence is a real error

```yaml
version:
  macos: latest
  # no default
```

On Linux this is a **resolve-time error** naming the arms that exist and the
active target set, not a silent fallthrough.

The mandatory-`default` alternative was rejected under review: it makes "a
fact that is not true yet" indistinguishable from "a machine the author
decided about". The first-ever apply on a fresh box — the one run that
matters — would silently take the catch-all, which is precisely the failure
this design exists to prevent.

### 6.5 What is not in the mechanism

`fromScript`, matcher `script:`, and `exitCode:` are not in the schema. The
reason is ordering, not purity:

> A script must run before the DAG in order to compute the diff. On a freshly
> imaged box that means it runs before Bedouin has installed anything, so
> `fromScript: doctor determine-version` can never see a tool Bedouin itself
> installs. On the machine class this tool exists for, the script branch is
> dead code and `fallback:` *is* the value — an elaborate mechanism whose only
> real behavior on a fresh machine is its default, with the config's most
> important number hidden behind a decorative key.

Deferred to v2 as a declared `sources:` block: named, top-level, mandatory
default, argv rather than shell, timeout, resolved once during fact
resolution and frozen into the plan artifact (§7.3) so `apply` never re-reads
it. Confining the impurity to one visible block is what makes it reviewable;
scattering it across arbitrary values is what makes it not.

`fromEnv` is also rejected — permanently, not deferred. It is
`{{ env.ZELLIJ_VERSION | default('latest') }}` and minijinja already does it.
Both rejected keys are in the did-you-mean table so that writing one produces
a message naming the replacement.

### 6.6 Which fields are evaluatable

Every scalar-or-list leaf: `version`, `from`, `path`, `installer`, `src`,
`dest`, `file`, `content`, and each `vars` value.

Not evaluatable: `version:` (the schema version), `includes:`, `targets:`
itself, and `name:` on a package. A package's name is its state-file
identity; a name that varies by machine is an identity that varies by
machine, and uninstall stops working. A tool packaged as `fd` on brew and
`fd-find` on apt is expressed through `from:` plus per-manager aliases in M2,
or as two declarations until then.

### 6.7 Rust representation

```rust
/// Depth is 1 by construction: the arm payload is T, never Value<T>.
/// Nested conditionals are unrepresentable rather than forbidden.
pub enum Value<T> {
    Const(T),
    ByTarget { arms: Vec<(ArmName, T)>, default: Option<T> },
}
```

`Deserialize` is hand-written, not `#[serde(untagged)]`. Untagged enums emit
`data did not match any variant of untagged enum`, and a config tool's error
messages are its user interface. The visitor dispatches on the YAML node
kind: a mapping means arms, everything else means `Const`.

Known arm names reach the visitor through a `scoped_thread_local` set
populated by stage 3, so name validation and did-you-mean happen inside
deserialization where the `file:line:col` span is still available.

Two coercions the visitor handles deliberately:

- `visit_f64` is **rejected**, not coerced. YAML reads `version: 1.80` as the
  float `1.8`, and by the time a visitor sees it the original text is gone —
  it would silently install the wrong version. The error says to quote it.
- `visit_i64`/`visit_u64` are accepted (`version: 3` is unambiguous).

`resolve(Value<T>, &Facts) -> Result<T>` is a pure function. It is the
single most test-dense unit in the crate and needs no `Host`.

## 7. `plan`

### 7.1 The DAG

Node order, which is also dependency order:

    package managers -> languages -> packages -> files -> rc blocks -> PATH

Edges beyond that ordering come from `from:`. A package with `from: cargo`
depends on the `rust` language node; a package with `from: brew` depends on
the brew manager node. `from: cargo` with no `rust` entry in `languages:`
adds one implicitly and warns — the warning is printed, the node is real, and
the plan shows it.

`from:` as a list is a fallback order resolved against the `managers` fact:
first manager that exists (or that Bedouin will have installed by that point
in the DAG) wins. If none will exist, that is a plan-time error naming the
package and the managers it asked for.

### 7.2 Diffing

Each node produces an item with a stable id (`package/zellij`,
`language/rust`, `file/~/.gitconfig`, `rc/70-zellij.zsh`, `path/~/.cargo/bin`).
The item is compared against:

- **state.json** — for ownership and last-applied version/hash.
- **the machine** — a read-only probe for what is actually installed.

Four outcomes: create, update, remove, no-op. Remove fires only for items
present in state with `owner: bedouin` and absent from config. An item
present on the machine but not in state is recorded `owner: preexisting` and
never removed, only adopted.

Read-only probes during `plan` run **Bedouin's own commands** — `brew list
--versions`, `cargo install --list`, `dpkg-query -W`, `rustup toolchain
list`. This is not a contradiction of §6.5: the prohibition is on executing
*user-supplied* code from the config, which is unreviewable and, on a fresh
box, always falls through to its default. Bedouin's probes are fixed,
auditable, and degrade to "not installed" when the manager is absent.

### 7.3 The plan artifact

`plan` resolves facts once and freezes them, together with the resolved item
list, into a plan artifact.

```
bedouin plan                 # resolve, print, discard
bedouin plan -o plan.json    # resolve, print, write the artifact
bedouin apply                # resolve and execute in one process
bedouin apply -f plan.json   # execute a previously reviewed artifact
```

The artifact records the resolved facts *including the environment*, so a
plan reviewed in one terminal applies identically in another. Without it,
`{{ env.X }}` and `match: { env: … }` reintroduce exactly the plan/apply
divergence that §6.5 rejects scripts to avoid — env is process-scoped and
otherwise unpersisted.

`apply -f` re-checks that the machine still matches the artifact's assumed
state and refuses on mismatch rather than applying a stale plan.

### 7.4 Output

```
Bedouin will make the following changes:

  + manager   brew                                    (not installed)
  + language  rust        1.80                        rustup
  ~ package   zellij      0.39.2 -> 0.40.1            cargo
  + package   jq          latest                      apt
  ~ file      ~/.gitconfig                            from templates/gitconfig.j2
  + rc        ~/.zshrc.d/70-zellij.zsh
  - package   ripgrep                                 was: apt, owner: bedouin
  + path      ~/.cargo/bin

Plan: 5 to add, 2 to change, 1 to remove.
```

`-v` annotates every value that came from a conditional with the arm that
won: `version = "nightly"   (target: macos-arm64)`. This is the only visible
trace of arm selection, and it is the thing a user reaches for when a config
resolves differently than expected.

`--dry-run` on `apply` is an alias for `plan`.

## 8. `apply`

### 8.1 Step environment

Every step is spawned with an environment Bedouin constructs, never the
inherited one. `PATH` is assembled from the state manifest's recorded bin
directories plus a minimal system base, so a step that needs `cargo` gets
`~/.cargo/bin/cargo` because a previous step recorded it there.

This is the fix for the Ansible reload problem and it is not an optimization:
it is why installing Rust and installing a cargo package can happen in one
run.

### 8.2 sudo

`sudo -n true` at fact resolution classifies the machine as `none`,
`passwordless`, or `password`. Steps declare whether they need root. On a
`password` machine, `apply` prompts **once**, up front, before any step runs,
listing the steps that will need it — not per-step, halfway through, after
twenty minutes of compiling.

apt and zypper steps are batched into a single privileged invocation per
manager. On a `none` machine, plan items requiring root are reported as
blocked with the reason, and `apply` refuses to start rather than failing
partway.

### 8.3 Failure semantics

`apply` is **not transactional** and does not pretend to be. Rolling back a
package manager is not something Bedouin can do honestly.

- Steps commit to state as they succeed. State is flushed after each step, so
  an interrupted run leaves a truthful record.
- On failure, `apply` **stops**. It does not continue to dependent steps, and
  it does not continue to independent ones either in v1 — a half-configured
  machine that reports success is worse than one that reports where it broke.
- Exit is nonzero. The failed step, its captured stderr, and the list of
  steps not attempted are printed.
- Re-running `apply` resumes: completed items diff as no-ops.

`--keep-going` is deferred. It is a real want for long runs, but it needs a
failure-summary design that M1 does not need to block on.

## 9. Managed blocks and PATH

rc content is written between sentinels:

    # >>> bedouin: zellij >>>
    eval "$(zellij setup --generate-auto-start zsh)"
    # <<< bedouin: zellij <<<

Content between markers is owned by that config entry. The rendered hash is
recorded in state; a mismatch is drift, flagged by `doctor` in M2 and lifted
back into the config by `absorb` in M4. In M1 a drifted block is **overwritten
by `apply` after printing what it replaced**, and the replaced text is kept in
the state file's `superseded` field so nothing is unrecoverably lost.

PATH is never string-edited. Bedouin renders exactly one file,
`{shell.rc_dir}/00-bedouin-path.{shell.name}`, from the structured `path:`
entries across all packages. Provenance and removal are automatic: drop the
package from the config and its PATH entry disappears with it.

The same sentinel mechanism ensures `shell.rc_file` sources `shell.rc_dir`
(§3.1), as a block owned by Bedouin itself rather than by any package.

## 10. State

`~/.local/state/bedouin/state.json`:

```json
{
  "schema_version": 1,
  "machine_id": "…",
  "last_apply": "2026-08-30T11:04:22Z",
  "items": {
    "package/zellij": {
      "owner": "bedouin",
      "version": "0.40.1",
      "method": { "manager": "cargo" },
      "files": [],
      "rc_blocks": [
        { "file": "~/.zshrc.d/70-zellij.zsh",
          "marker": "zellij",
          "hash": "sha256:…",
          "superseded": null }
      ],
      "path": ["~/.cargo/bin"],
      "resolved_from": { "version": "default", "from": "default" }
    }
  }
}
```

`owner` is what makes uninstall safe: removing an entry from the config
removes only `owner: bedouin` artifacts. A `jq` that was already on the
machine when Bedouin first ran is `preexisting` and survives.

`resolved_from` records which arm won for each conditional field. It costs
almost nothing and it is what lets M2's `doctor` say "this resolved
differently than last apply" — the failure mode that a conditional config
otherwise makes invisible.

`method` is recorded rather than assumed, so a package that moves from `apt`
to `cargo` between applies is removed and reinstalled rather than
double-installed.

## 11. The Host seam and testing

All I/O in `bedouin-core` goes through one trait:

```rust
pub trait Host {
    fn run(&self, cmd: &Cmd) -> Result<Output>;
    fn which(&self, bin: &str) -> Option<PathBuf>;
    fn read(&self, p: &Path) -> Result<Option<Vec<u8>>>;
    fn write(&self, p: &Path, bytes: &[u8], mode: u32) -> Result<()>;
    fn remove(&self, p: &Path) -> Result<()>;
    fn mkdir_p(&self, p: &Path) -> Result<()>;
    fn env(&self) -> &BTreeMap<String, String>;
}
```

Two implementations: `OsHost` for real runs, `FakeHost` for tests — an
in-memory filesystem plus a scripted command table that records invocations
and returns canned exit codes and output.

Three test layers:

1. **Pure unit tests, no Host.** Arm selection, specificity, tie detection,
   the deserializer's error messages, DAG construction, diffing. `resolve()`
   is a pure function of (config, facts) and carries the densest coverage.
2. **`FakeHost` integration tests.** Whole `plan` and `apply` runs against a
   simulated fresh machine. This is what makes the fresh-box path — the one
   nobody can test by hand repeatedly — actually testable, including the
   failure paths: manager missing, command exits nonzero, command times out,
   command prints garbage, sudo unavailable partway.
3. **Docker smoke tests.** One image per distro (`ubuntu:24.04`,
   `opensuse/tumbleweed`), running a real `apply` against a real package
   manager, asserting the binary exists and the rc file is sourced. Slow,
   few, and CI-only. macOS is covered by a CI runner, not a container.

Layer 2 is where the behavioral confidence lives. Layer 3 exists to catch the
lies in layer 2's fakes.

## 12. Errors

Every parse error carries `file:line:col` and, where a name was involved, a
did-you-mean over the known set. The rejected keys `fromEnv`, `fromScript`,
`script`, `exitCode`, and `matcher` are in that table with messages naming
their replacement rather than merely reporting them unknown.

    bedouin.yaml:14:7: unknown target `mcaos`
       |
    14 |       mcaos: nightly
       |       ^^^^^ did you mean `macos`?
       |
       = known targets: macos, linux, ubuntu, …, and 1 declared: work

Caret rendering via `annotate-snippets` is a nice-to-have; `file:line:col`
plus the hint is sufficient for M0.

## 13. Milestone split

**M0** — crates, schema types, the hand-written `Value<T>` deserializer,
facts resolver, the six-stage loader, `resolve()`, DAG construction, diffing
against an empty or existing state, `plan` output, plan artifact write.
`Host` exists and `FakeHost` is used throughout. Nothing mutates the machine.

**M1** — `OsHost`, the executor for managers/languages/packages/files/rc
blocks/PATH, sudo handling, state store reads and writes, `apply` and
`apply -f`, failure semantics, docker smoke tests, musl and universal release
builds.

## 14. Deferred, with reasons

| Deferred | Until | Why not now |
|---|---|---|
| `sources:` / any plan-time execution | v2 | §6.5 — on a fresh box the script branch is dead code |
| `fromEnv` | never | `{{ env.X \| default(…) }}` |
| Nested conditionals | never | unrepresentable by construction (§6.7) |
| Vars referencing vars | never | keeps resolution two flat layers, not a fixpoint |
| Per-manager package aliases | M2 | SUSE support is what forces the issue |
| `--keep-going` | post-M1 | needs a failure-summary design |
| `doctor`, drift, `remove` | M2 | |
| Tauri app | M3 | webkit2gtk must stay off the bootstrap path |
| `absorb`, `reconcile --watch` | M4 | |
