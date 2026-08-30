# Bedouin M0+M1 — design

Status: draft for review (revision 2)
Scope: M0 (schema, facts, `plan`) and M1 (executor, state, `apply`)
Extends `bedouin-handoff.md`. Where this document departs from the handoff,
§15 says so explicitly.

Revision 2 applies the findings of a six-lens adversarial review. The
substantive changes are: bin-directory recording (§8.1, §10), the `only:` key
(§6.6), specificity by implied fact set (§6.3), a declared `shell:` (§3.1),
a loader reordered to preserve error spans (§4), config-root and path
normalization rules (§4.1), and the state store's durability rules (§10.2).

## 1. Scope

M0 and M1 ship one binary that takes a machine from freshly imaged to fully
configured, and reports honestly what it would do before it does it.

In scope: schema v0 including `includes:` and conditional values; the facts
resolver; config loading, arm selection and rendering; `plan` (DAG, diff,
terraform-style output, plan artifact); `apply` (managers, languages,
packages, files, rc blocks, PATH); the state store. Ubuntu and macOS are
tested execution targets. **SUSE became a tested execution target in M2**: a
real `apply` against real zypper on `opensuse/tumbleweed` is part of the
smoke suite.

Out of scope, in expected order of arrival:

- `init`, `add`, `sync` — M1.5, after `apply` is trustworthy. They are
  config-editing and git conveniences over a working core, and none of them
  changes the engine. (The handoff lists them under v1; this assigns them a
  milestone, it does not drop them.)
- `doctor`, drift reporting, the `remove` **command**, SUSE — M2. Note that
  removal *as a plan outcome* is in M1: dropping a package from the config
  and re-applying removes it. What M2 adds is the imperative shortcut.
- Tauri app — M3. `absorb`, `reconcile --watch` — M4.
- **All execution of user-supplied code during `plan`** — `sources:`,
  `fromScript`, matcher `script:`. §6.5 explains why this is a correctness
  decision rather than a scheduling one.

### 1.1 Trust model

`bedouin.yaml`, its includes, and the templates under `src:` are **trusted
input**: they are the user's own configuration, versioned in the user's own
repository, and Bedouin executes what they declare. Bedouin does not sandbox
them and does not try to.

What Bedouin does owe the user, and what §8 and §9 are written to provide:

- No *surprising* writes. Every path Bedouin touches is derivable from the
  config, and paths that escape the config root or the home directory are
  rejected rather than silently followed (§4.1).
- No *accidental* escalation. Root is used only where a step declares it, and
  the plan says which steps those are before any of them runs (§8.2).
- No secret leakage into artifacts. The plan artifact and state file record
  only what they need, at mode 0600 (§7.3, §10.2).

A config pulled by `sync` from a repository the user does not control is
outside this model, and `sync` (M1.5) will show a diff before applying.

## 2. Crate layout

    bedouin-core/     schema, facts, loader, resolver, planner, executor,
                      state store. All logic. No I/O except through Host.
    bedouin-cli/      clap wrapper. Static binary. The only thing that runs
                      on a fresh machine.

`bedouin-app` (Tauri) is not created until M3: a webkit2gtk-dependent crate
must not sit in the workspace the bootstrap path builds.

Release targets: `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`
on Linux, universal binary on macOS. Neither musl target is installed on the
current dev machine; `rustup target add` is an M1 release prerequisite.

**YAML crate: `serde_yaml_ng`.** `serde_yaml` is archived and unmaintained,
and §4's span-preserving loader depends on the crate exposing node locations.
This is pinned here because the choice is load-bearing rather than incidental.

## 3. Facts

Facts are resolved by the engine, never declared. The user asks "where is the
rc dir" instead of writing conditionals about it.

| Fact | Type | Notes |
|---|---|---|
| `os` | `macos` \| `linux` | |
| `distro` | `ubuntu` \| `debian` \| `fedora` \| `opensuse` \| `arch` \| `other` | `/etc/os-release` `ID`; `macos` on macOS |
| `distro_like` | `debian` \| `rhel` \| `suse` \| `arch` \| `none` | `ID_LIKE`, falling back to `ID` |
| `distro_version` | string | `VERSION_ID`, e.g. `24.04`; macOS product version |
| `arch` | `x86_64` \| `arm64` | |
| `home`, `user`, `hostname` | | `hostname` is the short name |
| `shell.name` | `zsh` \| `bash` \| `fish` \| `other` | §3.1 |
| `shell.rc_file`, `shell.rc_dir` | path | §3.1 |
| `privilege` | `root` \| `passwordless` \| `password` \| `unavailable` | §8.2 |
| `env` | map | process environment |
| `managers` | set | which of brew/apt/zypper/dnf/mise/cargo/rustup exist |

`distro: other` and `shell.name: other` are deliberate. An unrecognized
machine must be representable, or Bedouin cannot run at all somewhere it has
not been taught about. Both are addressable as arms (§6.1).

### 3.1 Shell: detected, and declarable

The detected shell is the pre-Bedouin shell. On the fresh-box case this tool
exists for, that is frequently **not** the shell the user is configuring —
they are installing zsh in this very run, from bash.

So the schema carries a top-level declaration:

```yaml
shell: zsh          # optional; defaults to the detected login shell
```

`shell.name`, `shell.rc_file`, and `shell.rc_dir` derive from the *declared*
shell. The detected shell remains available as `shell.detected` for the rare
config that needs it. Declaring a shell that no package in the config
installs, and that is not already present, is a plan-time warning naming both
facts.

`shell.rc_dir` is the drop-in directory: `~/.zshrc.d`, `~/.bashrc.d`,
`~/.config/fish/conf.d`. Bedouin creates it if absent and, for zsh and bash,
ensures the rc file sources it via a managed block (§9). Fish sources
`conf.d` natively and needs no block. For `shell.name: other`, rc and PATH
nodes are a plan-time error naming the shell — Bedouin will not guess at an
unknown shell's syntax.

The DAG orders the shell's own package before every rc and PATH node (§7.1),
so the run that installs zsh also writes into `~/.zshrc.d`.

`plan` reports `+ create ~/.zshrc.d` and `+ managed block in ~/.zshrc` as
plan items. It does not create them; `apply` does.

### 3.2 Facts Bedouin itself changes are not matchable

`managers` and `shell` are **excluded from the arm vocabulary** (§6.1).
Matching on them is circular: Bedouin installs package managers and shells,
so an arm keyed on one is chosen from the pre-Bedouin state and is wrong on
exactly the fresh-box case. The declared `shell:` of §3.1 is the supported
way to express shell intent, and it is an input rather than a fact.

Both remain readable in templates, where the same hazard exists but is
visible at the point of use.

## 4. Config loading pipeline

Seven ordered stages. Stage 1 is the only one that touches the world.

    1. locate    find the config root and the top-level file (§4.1)
    2. read      top-level file + includes: globs, in declaration order
    3. parse     EACH file -> untyped spanned document. Spans survive.
    4. collect   scan all documents for `targets:`; build the name set
    5. typed     deserialize each document with the name set in scope
    6. merge     concatenate list sections; duplicate item ids are an error
    7. resolve   prune `only:` -> select Value<T> -> render minijinja
    -> Config, fully concrete, containing no conditionals

**Parse-per-file before merge (stages 3–6) is what preserves error spans.**
Merging YAML *text* first, as revision 1 specified, destroys the
`file:line:col` that §6.7 and §12 depend on — the ordering that is
load-bearing is parse-then-merge, not merge-then-parse.

**Collect before typed deserialize (stages 4–5) is what makes `includes:`
work.** A target declared in `conf.d/10-targets.yaml` must be in scope when
`conf.d/20-packages.yaml` is deserialized. Collecting names per file would
reject configs that are correct as a whole.

**Prune before select before render (stage 7)** means a pruned item's other
fields are never resolved — so `only: linux` on an entry whose `from:` is
`{ubuntu: apt}` does not trip §6.4's no-default error on a Mac — and losing
arms are never rendered, so a `{{ home }}/.cargo/bin` inside a `ubuntu:` arm
costs nothing on macOS.

Stage 7 completes before the planner runs. **The planner and the state file
never see a conditional value.** That is what keeps conditionals out of the
diff, the DAG, and the state schema.

### 4.1 Config root, search order, and path normalization

    --config <path>  ->  $BEDOUIN_CONFIG  ->  ./bedouin.yaml
                     ->  ~/.config/bedouin/bedouin.yaml

The **config root** is the directory containing the resolved top-level file.
`includes:` globs and `files[].src` are resolved against the config root, not
the process working directory, so `bedouin apply` behaves identically from
any cwd. A `src:` or include that escapes the config root is an error.

Glob expansion within a single `includes:` entry is sorted lexicographically,
so `conf.d/*.yaml` has a defined order rather than a filesystem-dependent
one. That is what makes `10-`/`20-` prefixes mean what they look like.

Every path the config produces — `dest:`, `rc[].file`, `path[]` — is
**normalized at the end of stage 7**: `~` expands to `home`, relative paths
resolve against the config root, `.`/`..` collapse, and the result is
absolute. **Item ids are built from the normalized path**, so `~/.gitconfig`,
`$HOME/.gitconfig`, and `/home/u/.gitconfig` are one item rather than three.
A `dest:` outside `home` is permitted only when the step declares root
(§8.2); a `dest:` that resolves outside both `home` and the declared root
paths is an error.

The config root is recorded in the plan artifact.

## 5. Schema v0

```yaml
version: 0                    # schema version, not a bedouin version

includes:                     # optional; globs, relative to the config root
  - conf.d/*.yaml

shell: zsh                    # optional; the shell being configured (§3.1)

vars:
  editor: nvim

targets:                      # named conditions; declaration order matters
  - name: work
    match: { env: { BEDOUIN_PROFILE: work } }
    vars: { editor: vim }

package_managers:             # evaluatable (§6.6)
  macos: [brew, mise]
  default: [apt, mise]

languages:
  - name: rust
    version: "1.80"
    installer: rustup

packages:
  - name: zellij
    from: cargo               # implies rust; auto-added with a warning
    version: latest
    needs: [build-essential]  # explicit DAG edge (§7.1)
    path: ["{{ home }}/.cargo/bin"]
    rc:
      - file: "{{ shell.rc_dir }}/70-zellij.zsh"
        content: |
          eval "$(zellij setup --generate-auto-start zsh)"

  - name: xclip
    from: apt
    only: linux               # membership conditional (§6.6)

  - name: jq
    from: { macos: brew, debian-like: apt, suse-like: zypper }

files:
  - src: templates/gitconfig.j2
    dest: ~/.gitconfig
    mode: "0644"              # optional; §9.1
```

Unknown keys are rejected with a did-you-mean (§12). A config tool that
silently ignores a misspelled key silently does the wrong thing.

## 6. Evaluatable values

One sentence: **a YAML mapping where a value is expected means branches;
anything else is the literal value.**

```yaml
packages:
  - name: fd
    from:
      macos: brew
      default: [apt, zypper]     # a list is still just a value
    version: latest              # a scalar is still just a value
```

No `Value` keyword, no `select:`, no `when:`, no `matcher:`. The shape of the
YAML carries the meaning, so the common case stays one line and the
conditional case stays four.

### 6.1 Arm names

Arm keys come from a **closed vocabulary**: built-in names plus names
declared under `targets:`. A key outside that set is a parse error with a
did-you-mean.

This is the property the design is bought for: the vocabulary does not depend
on the machine, so **a config is valid or invalid identically everywhere** —
only which arm *wins* varies. A typo cannot become a branch that silently
never matches on a machine you are not sitting at.

Built-in names, and the facts each **implies**:

| Arm | Implies |
|---|---|
| `macos` | os=macos |
| `linux` | os=linux |
| `ubuntu` | distro=ubuntu, distro_like=debian, os=linux |
| `debian` | distro=debian, distro_like=debian, os=linux |
| `fedora` | distro=fedora, distro_like=rhel, os=linux |
| `opensuse` | distro=opensuse, distro_like=suse, os=linux |
| `arch` | distro=arch, distro_like=arch, os=linux |
| `other-distro` | distro=other, os=linux |
| `debian-like`, `rhel-like`, `suse-like`, `arch-like` | distro_like=…, os=linux |
| `x86_64`, `arm64` | arch=… |
| `{name}-{arch}` for every name above | the union of both |

Not included, per §3.2: shell names, manager names.

The implication column is not documentation — §6.3 computes on it.

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
`hostname`, `env`. Scalars match exactly, lists match any, and strings
beginning with `>=`, `>`, `<=`, `<` compare as versions. An empty `match: {}`
is a parse error — write `default:`. A target name colliding with a built-in
is a parse error.

`distro_version` is why the vocabulary cannot be closed-and-only-closed: the
most common reason a bootstrap config needs a conditional at all is "Ubuntu
22.04 ships an unusably old X", and versions are not enumerable. Declaring a
target keeps arm *names* closed while leaving the *axes* open, and needs no
Bedouin release to address a new machine class.

### 6.3 Selection

Three rules, applied in order.

**1. A declared target beats every built-in.** You named it deliberately;
that is the signal. `{ noble: apt, ubuntu: cargo }` on Ubuntu 24.04 resolves
to `apt`.

**2. Among declared targets, `targets:` declaration order wins.** First
declared, first served. Co-occurrence of open predicates (`distro_version
>= 24.04` versus `hostname: khaymah`) is not decidable in general, so
Bedouin does not pretend to decide it — it uses the order the user wrote,
which is visible in one place at the top of the file. This also restores the
handoff's stated `targets:` semantics (§15).

**3. Among built-ins, a strictly refining arm wins; arms that neither
refines are a parse error.**

Compare arms by their **implied** fact sets from §6.1's table, by subset
inclusion rather than by size. If one arm's implied set is a strict superset
of another's, it refines it and wins. If two active arms' sets are
incomparable, that is a parse error — even when one set is larger. This
matters:

```yaml
from:
  ubuntu: apt          # implies 3 facts (distro, distro_like, os)
  linux: cargo         # implies 1 fact  (os)
  default: brew
```

`ubuntu` strictly refines `linux`, so on Ubuntu it wins and the config means
what it obviously means. Under revision 1's "count the pinned facts" rule
both arms scored 1, co-occurred, and the file was rejected at parse time with
no expressible fix — "apt on Ubuntu, cargo on other Linuxes" was unwritable.
That was the sharpest bug the review found.

Only **incomparable** arms tie:

```yaml
version:
  macos: "1.80"        # implies {os}
  arm64: nightly       # implies {arch} — neither set contains the other
  default: stable
```

This is a parse error naming both arms and suggesting `macos-arm64`, which
exists in the vocabulary precisely for it.

The suggested conjunction is the arm pinning *exactly* the union of the two,
never merely a superset: `ubuntu-arm64` also contains both `debian-like` and
`arm64`, but proposing it would quietly drop Debian from an arm the user wrote
for the whole family.

Subset inclusion rather than set size is load-bearing, and a size rule looks
right until it isn't: `{ debian-like: apt, arm64: cargo }` implies sets of
size 2 and 1 that are nonetheless **disjoint**, and both are active on a
Debian ARM box. Under a size rule `debian-like` would win silently — exactly
the shadowing class this section exists to eliminate, reintroduced across
axes. Under subset inclusion it is the parse error it should be. Where no
conjunction exists in §6.1's vocabulary for the pair (there are no
names for the pair), the error says to declare a target instead.

Written order of arms is irrelevant throughout. The map reads like a set
because it is one, and a formatter that sorts keys cannot change meaning.
Under written-order selection, `{macos: "1.80", macos-arm64: nightly}` would
make the nightly arm unreachable on every machine, silently.

**Target `vars:` fold by the same rules, per key.** Active targets' `vars`
blocks are merged key by key into the base `vars:`; where two active targets
set the same key, rule 2 decides and the first-declared target wins. Merging
is per key rather than wholesale, so a target that sets only `editor` does
not drop another active target's unrelated `proxy`. Revision 1 left this
undefined entirely, which meant two implementers would produce different
renderings of the same config.

### 6.4 `default` is optional, and its absence is a real error

```yaml
version:
  macos: latest
  # no default
```

On Linux this is a **resolve-time error** naming the arms that exist and the
active target set — not a silent fallthrough.

Mandatory-`default` was rejected: it makes "a fact that is not true yet"
indistinguishable from "a machine the author decided about", so the
first-ever apply on a fresh box would silently take the catch-all. That is
the failure this design exists to prevent.

Note the division of labour with §6.6's `only:`: a missing `default:` means
*the author did not decide*; `only:` means *the author decided the item does
not exist here*. Both must be expressible, and they must not look alike.

### 6.5 What is not in the mechanism

`fromScript`, matcher `script:`, and `exitCode:` are not in the schema. The
reason is ordering, not purity:

> A script must run before the DAG in order to compute the diff. On a freshly
> imaged box that means it runs before Bedouin has installed anything, so
> `fromScript: doctor determine-version` can never see a tool Bedouin itself
> installs. On the machine class this tool exists for, the script branch is
> dead code and `fallback:` *is* the value — an elaborate mechanism whose
> only real behavior on a fresh machine is its default, with the config's
> most important number hidden behind a decorative key.

Deferred to v2 as a declared `sources:` block: named, top-level, mandatory
default, argv rather than shell, timeout, resolved once during fact
resolution and frozen into the plan artifact (§7.3). Confining the impurity
to one visible block is what makes it reviewable; scattering it across
arbitrary values is what makes it not.

`fromEnv` is rejected permanently, not deferred: it is
`{{ env.ZELLIJ_VERSION | default('latest') }}`. Both rejected keys are in the
did-you-mean table, so writing one produces a message naming the replacement.

### 6.6 What is evaluatable, and `only:`

**Evaluatable leaves:** `version`, `from`, `installer`, `path`, `src`,
`dest`, `mode`, `file`, `content`, `needs`, `package_managers`, and each
`vars` value.

**Not evaluatable, with reasons:**

- `version:` (the schema version) and `includes:` — arms resolve at stage 7,
  but stages 4–6 need the full file set already loaded. Evaluatable includes
  invert the loader's ordering.
- `targets:` itself — it is the thing arms are defined against.
- `name:` on a package — it is the state-file identity. A name that varies by
  machine is an identity that varies by machine, and uninstall stops working.
  A tool packaged `fd` on brew and `fd-find` on apt is expressed through
  per-manager aliases in M2, or two `only:`-gated declarations until then.

**`only:` — the membership conditional.** Arms choose between values; they
cannot make an item not exist. Without a way to say "not on this platform",
one config in git cannot cover Ubuntu and macOS, which is the product
premise. Four of the six review lenses found this independently.

```yaml
packages:
  - name: xclip
    from: apt
    only: linux                    # one arm name, or a list of them
  - name: mas
    from: brew
    only: [macos]
```

Valid on `packages`, `languages`, and `files`. The payload is one arm name or
a list, drawn from the same §6.1 closed vocabulary with the same did-you-mean
on a typo, OR-ed together. No nesting, no negation — declare a target
instead. No specificity applies: there is no winner to pick.

`only:` is evaluated **first** in stage 7, and a pruned item's other fields
are never resolved. Without that ordering the fix does not work:
`only: [ubuntu, opensuse]` on an entry whose `from:` is
`{ubuntu: apt, opensuse: zypper}` would still trip §6.4's no-default error on
macOS. Pruning also interacts correctly with §7.2 removal — state is
per-machine, so a pruned item was never in this machine's state and no
spurious `- package` is emitted.

**`package_managers:` is evaluatable**, and a declared manager that cannot
exist on the resolved OS is dropped from the DAG rather than planned. Only
brew, mise, rustup, and cargo have bootstrap recipes; apt and zypper are
distro-provided and are never installed by Bedouin. `-v` reports each drop.
No applicability table is needed: if dropping leaves some package's `from:`
with no viable manager, §7.1's existing plan-time error fires, naming the
package and the managers it asked for.

### 6.7 Rust representation

```rust
/// Depth is 1 by construction: the arm payload is T, never Value<T>.
/// Nested conditionals are unrepresentable rather than forbidden.
pub enum Value<T> {
    Const(T),
    ByTarget { arms: Vec<(ArmName, T)>, default: Option<T> },
}
```

Fields are concrete instantiations — `Value<String>`,
`Value<OneOrMany<String>>` — so no type parameter or trait bound propagates
into the schema structs. The generic stops at the field.

`Deserialize` is hand-written, not `#[serde(untagged)]`. Untagged enums emit
`data did not match any variant of untagged enum`, and a config tool's error
messages are its user interface. The visitor dispatches on YAML node kind: a
mapping means arms, everything else means `Const`.

Known arm names reach the visitor through a `scoped_thread_local` set
populated by stage 4, so name validation and did-you-mean happen inside
deserialization while the span is still available.

Three deliberate coercion rules, applied on **both** the `Const` path and
each arm payload — revision 1 applied them only to `Const`, which let
`{macos: 1.80, default: latest}` through:

- `f64` is **rejected**. YAML reads `version: 1.80` as the float `1.8`, and
  by the time a visitor sees it the original text is gone — it would silently
  install the wrong version. The error says to quote it.
- `i64`/`u64` are accepted; `version: 3` is unambiguous.
- `bool` is rejected; YAML 1.1's `on`/`no` reaching a version field is a typo,
  not an intention.

`resolve(Value<T>, &Facts) -> Result<T>` is a pure function and the most
test-dense unit in the crate. It needs no `Host`.

**minijinja runs with `UndefinedBehavior::Strict`.** The default is lenient:
`{{ hom }}/.cargo/bin` renders to `/.cargo/bin` and ships a wrong PATH entry
silently. Strict mode makes it an error, which is the only setting consistent
with §6.1's whole argument about typos.

**Template context**, defined once here because revision 1 left it to be
inferred from examples: facts are bare (`os`, `arch`, `home`, `user`,
`hostname`, `distro`, `distro_like`, `distro_version`, `shell.*`), user
variables are namespaced (`vars.*`), and the environment is namespaced
(`env.*`). Nothing else is in scope.

## 7. `plan`

### 7.1 The DAG

Stage order, which is also dependency order:

    package managers -> languages -> shell package -> packages
                     -> files -> rc blocks -> PATH

The shell's own package is pulled ahead of the general package stage so the
run that installs zsh can write into `~/.zshrc.d` (§3.1).

Edges beyond stage order:

- `from:` — `from: cargo` depends on the `rust` language node; `from: brew`
  depends on the brew manager node. `from: cargo` with no `rust` entry adds
  one implicitly and warns; the warning is printed and the node is real.
- **`needs:`** — an explicit edge to another package in the same config, for
  build prerequisites the DAG cannot infer. `zellij` from cargo needs a C
  toolchain, and nothing in `from: cargo` says so. Revision 1 had no
  inter-package edges at all, so `build-essential` and `zellij` sat in one
  unordered stage.
- Within a stage, with no edge to separate them, **declaration order**.
  Deterministic beats arbitrary, and it is the order the user can see.

A cycle in `needs:` is a plan-time error naming the cycle.

`from:` as a list is a fallback order resolved against the `managers` fact
**as of that point in the DAG** — a manager Bedouin will have installed by
then counts as available. If none will exist, that is a plan-time error
naming the package and the managers it asked for.

### 7.2 Diffing

Each node produces an item with a stable id built from the normalized path or
name (§4.1): `package/zellij`, `language/rust`, `file//home/u/.gitconfig`,
`rc/zellij/70-zellij.zsh`, `path//home/u/.cargo/bin`. The rc id carries the
owning package because two packages may write files of the same basename.

**Duplicate ids are a parse error** naming both source files. Revision 1's
"concatenate list sections" let two includes declare the same package,
producing two DAG nodes competing for one state key.

Items compare against two sources:

- **state.json** — ownership, last-applied version, hashes.
- **the machine** — a read-only probe of what is installed.

Four outcomes: create, update, remove, no-op. Remove fires only for items in
state with `owner: bedouin` that are absent from the config. An item present
on the machine but not in state is recorded `owner: preexisting` and is never
removed, only adopted.

**`version: latest` means "install if absent; never upgrade automatically."**
Diff: absent → create, present → no-op, regardless of what the registry now
holds. This is what keeps `plan` deterministic, offline, and fast; it is also
what makes the plan you reviewed the plan that applies. Upgrading is an
explicit `bedouin upgrade` (post-M1). A pinned `version: "1.80"` diffs
against the installed version and maps to per-manager syntax through the
installer recipe table (§8.4). Revision 1 left `latest` undefined and its
§7.4 example implied a network read that §7.2's probe list did not have.

Read-only probes during `plan` run **Bedouin's own commands** — `brew list
--versions`, `cargo install --list`, `dpkg-query -W`, `rustup toolchain
list`. This is not a contradiction of §6.5: the prohibition is on executing
*user-supplied* code from the config, which is unreviewable and, on a fresh
box, always falls through to its default. Bedouin's probes are fixed,
auditable, and degrade to "not installed" when the manager is absent.

### 7.3 The plan artifact

```
bedouin plan                 # resolve, print, discard
bedouin plan -o plan.json    # also write the artifact
bedouin apply                # resolve and execute in one process
bedouin apply -f plan.json   # execute a previously reviewed artifact
```

The artifact records: an artifact `schema_version`, the config root, the
resolved facts, the referenced environment (below), the resolved item list
with each item's diff outcome, and the state digest it was computed against.

**Only the environment variables the config actually references are frozen** —
the union of `match: { env: … }` keys and `env.X` occurrences in templates.
Revision 1 froze the whole process environment, which writes every secret in
the user's shell into a file made to be reviewed and shared. The artifact is
written mode 0600 regardless.

Freezing the referenced env is what makes `{{ env.X }}` and `match: {env:}`
safe: env is process-scoped and otherwise unpersisted, so without the
artifact a plan reviewed in one terminal applies differently in another —
exactly the divergence §6.5 rejects scripts to avoid.

`apply -f` re-checks **the state digest and the facts, not the environment**.
The environment is what the artifact exists to carry forward; re-checking it
would reject precisely the case the artifact is for. A facts or state
mismatch refuses the stale plan and says which fact moved.

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

`-v` annotates each value that came from a conditional with the arm that won
(`version = "nightly"   (target: macos-arm64)`), and reports items pruned by
`only:` and managers dropped as inapplicable. This is the only visible trace
of arm selection, and it is what a user reaches for when a config resolves
differently than expected.

Exit codes: **0** no changes pending, **2** changes pending, **1** error.
`plan` exiting 2 on pending changes is what makes it usable in a CI drift
check, which is the first thing anyone scripts it for. `--dry-run` on `apply`
is an alias for `plan`.

## 8. `apply`

### 8.1 Step environment and bin directories

Every step is spawned with an environment Bedouin constructs, never the
inherited one. `PATH` is assembled from a minimal system base plus **every
bin directory recorded in the state manifest**, so a step needing `cargo`
finds `~/.cargo/bin/cargo` because an earlier step recorded it.

This is the fix for the Ansible reload problem and the reason installing Rust
and a cargo package can happen in one run.

For that to work, bin directories must actually be recorded — and revision 1
recorded none. `path:` existed only on packages, so no language or manager
ever contributed anything, the manifest was empty on a first run, and the
headline feature could not work. Every review lens found this independently.

**Manager and language nodes record their bin directories into state from
their installer recipe (§8.4), not from user configuration.** The user should
not have to tell Bedouin where rustup puts cargo:

| Node | Recorded bin dirs |
|---|---|
| `language/rust` (rustup) | `{home}/.cargo/bin` |
| `language/go` (mise) | `{home}/.local/share/mise/shims` |
| `manager/brew` (macos-arm64) | `/opt/homebrew/bin` |
| `manager/brew` (linux) | `/home/linuxbrew/.linuxbrew/bin` |
| `manager/mise` | `{home}/.local/bin` |

A package's `path:` entries are additional and still user-declared; they are
what lands in the rendered PATH file (§9), which is a different question from
what a step's env needs.

### 8.2 Privilege

The `privilege` fact is four-valued because a three-valued one cannot be
computed. `sudo -n true` has two outcomes and cannot distinguish "no sudo
rights" from "sudo needs a password", yet revision 1 gated a refuse-to-start
decision on exactly that distinction.

    euid == 0                       -> root
    sudo -n true succeeds           -> passwordless
    in group sudo / wheel / admin   -> password
    otherwise                       -> unavailable

`root` matters in practice: containers run as root, and §11's layer-3 smoke
tests run in containers. Revision 1 had no `root` case, so `apply` would have
refused to start inside its own test harness.

Group membership rather than a second sudo probe, because `sudo -n -l` does
not work: it exits nonzero both when the user has no rights *and* when it
merely wants a password, which is exactly the distinction being drawn. This
was measured, not assumed -- on a machine in the `sudo` group with a password
required, `sudo -n true` and `sudo -n -l` both return 1.

Steps declare whether they need root. On `password`, `apply` validates once
up front (`sudo -v`), listing the steps that will need it, and then **holds
the credential with a keepalive** — a background `sudo -n true` every 60
seconds for the duration of the run. Without it the promise of a single
prompt is false: sudo's timestamp expires after 15 minutes by default, and
§8.3's own scenario is a twenty-minute compile. The keepalive stops when
`apply` exits.

<!-- ponytail: keepalive covers timestamp_timeout>0; on a hardened box with
     timestamp_timeout=0 every privileged step prompts. Detect and say so up
     front rather than surprising the user mid-run. -->

apt and zypper steps are batched into a single privileged invocation per
manager, passed as argv — never as a shell string. On `unavailable`, plan
items requiring root are reported as blocked with the reason and `apply`
refuses to start rather than failing partway.

### 8.3 Failure semantics

`apply` is **not transactional** and does not pretend to be. Rolling back a
package manager is not something Bedouin can do honestly.

- **Intent is recorded before the work, not after.** A step writes
  `status: incomplete, owner: bedouin` to state before it begins and flips it
  to `complete` when it succeeds. Revision 1 recorded only on success, so a
  run interrupted between "installed" and "flushed" left a package that
  Bedouin had installed looking `preexisting` — permanently un-removable,
  silently. An `incomplete` item re-diffs as needing work, and it is never
  adopted as preexisting.
- On failure, `apply` **stops**. It does not continue to dependent steps, and
  not to independent ones either in v1: a half-configured machine reporting
  success is worse than one reporting where it broke.
- Exit is nonzero. The failed step, its stderr, and the steps not attempted
  are printed.
- Re-running `apply` resumes; completed items diff as no-ops.

**Step output streams** rather than being captured silently. A twenty-minute
rustup or cargo build printing nothing is indistinguishable from a hang.
Output is prefixed with the step id, and `-q` reduces it to a spinner while
still retaining the tail for the failure report.

`--keep-going` is deferred; it needs a failure-summary design M1 need not
block on.

### 8.4 Installer recipes

A recipe is compiled-in, not user-supplied, and gives per manager or
installer: the probe command, the install command, the remove command, the
version-pin syntax, and the bin directories to record (§8.1). M1 ships
recipes for brew, apt, zypper, cargo, rustup, and mise.

`languages[].installer` accepts `rustup` and `mise` in M1. Any other value is
a parse error listing the supported installers — not a silent fallthrough to
a generic path that does not exist.

## 9. Managed blocks, files, and PATH

rc content is written between sentinels:

    # >>> bedouin: zellij >>>
    eval "$(zellij setup --generate-auto-start zsh)"
    # <<< bedouin: zellij <<<

Content between markers is owned by that config entry. Two protocol rules
revision 1 omitted, both of which leave login-shell code in an unowned state:

- **Rendered content containing a sentinel line is a render-time error.** A
  template that emits `# >>> bedouin: …` could otherwise split or capture a
  neighbouring block.
- **A start marker with no matching end marker is a hard error**, naming the
  file and line. Bedouin does not guess where the block ended and does not
  rewrite the file.

The rendered hash is recorded in state. A mismatch is drift: flagged by
`doctor` in M2, lifted back into config by `absorb` in M4. In M1 a drifted
block is overwritten after printing what it replaced, and the replaced text
is kept in state's `superseded` field.

<!-- ponytail: `superseded` is unbounded and may hold a secret the user
     hand-pasted into a block. M2's `doctor` should surface and expire it. -->

### 9.1 Managed files

`files[]` gains `mode:` (default `0644`, and `0600` for anything under
`~/.ssh` or `~/.gnupg`), parent directories are created, and:

- **An existing file at `dest` that is not in state is backed up** to
  `<dest>.bedouin-bak` before the first write, and the backup path is
  recorded in state. Revision 1 gave packages a `preexisting` protection and
  gave files nothing, so a first apply silently destroyed the user's
  `~/.gitconfig`.
- **Symlinks at `dest` are not followed.** Bedouin refuses and says so;
  writing through a symlink means writing somewhere the config does not name.
- The **rendered output is snapshotted** in state per managed file. The
  handoff requires this for M4's three-way absorb, and revision 1 dropped it;
  it is cheap now and unreconstructible later.

### 9.2 PATH

PATH is never string-edited. Bedouin renders exactly one file,
`{shell.rc_dir}/00-bedouin-path.{ext}`, from the structured `path:` entries
across all packages. Entries are ordered by package declaration order, then
by their order within a package — deterministic, and visible in the config.

Per-shell syntax, because the file is Bedouin's to write:

| Shell | Line |
|---|---|
| zsh, bash | `export PATH="<dir>:$PATH"` |
| fish | `fish_add_path <dir>` |

Provenance and removal are automatic: drop the package and its PATH entry
goes with it. The same sentinel mechanism makes `shell.rc_file` source
`shell.rc_dir` (§3.1), as a block owned by Bedouin itself.

## 10. State

### 10.1 Shape

`~/.local/state/bedouin/state.json`:

```json
{
  "schema_version": 1,
  "last_apply": "2026-08-30T11:04:22Z",
  "items": {
    "package/zellij": {
      "kind": "package",
      "owner": "bedouin",
      "status": "complete",
      "version": "0.40.1",
      "method": { "manager": "cargo" },
      "bin_dirs": [],
      "path": ["/home/u/.cargo/bin"],
      "rc_blocks": [
        { "file": "/home/u/.zshrc.d/70-zellij.zsh", "marker": "zellij",
          "hash": "sha256:…", "superseded": null }
      ],
      "resolved_from": { "version": "default", "from": "default" }
    },
    "language/rust": {
      "kind": "language", "owner": "bedouin", "status": "complete",
      "version": "1.80", "method": { "installer": "rustup" },
      "bin_dirs": ["/home/u/.cargo/bin"]
    },
    "file//home/u/.gitconfig": {
      "kind": "file", "owner": "bedouin", "status": "complete",
      "hash": "sha256:…", "render_snapshot": "…",
      "backup": "/home/u/.gitconfig.bedouin-bak", "mode": "0644"
    }
  }
}
```

Every item kind — `manager/`, `language/`, `package/`, `file/`, `rc/`,
`path/` — carries `kind`, `owner`, and `status`. Revision 1 showed only the
package shape, leaving five kinds to be invented.

`owner` is what makes uninstall safe: removing a config entry removes only
`owner: bedouin` artifacts. A `jq` already present when Bedouin first ran is
`preexisting` and survives.

`bin_dirs` is what §8.1 assembles the step PATH from. `resolved_from` records
which arm won per conditional field — nearly free, and what lets M2's
`doctor` say "this resolved differently than last apply", the failure a
conditional config otherwise makes invisible. `method` is recorded rather
than assumed, so a package moving from apt to cargo is removed and
reinstalled rather than double-installed.

`machine_id` is dropped: revision 1 carried it with no source, no consumer,
and no defined behavior on change.

### 10.2 Durability

Four rules, none of which revision 1 stated, and each of whose absence
degrades to "treat state as empty" — the outcome that erases ownership and
makes every Bedouin-installed package look preexisting.

- **Lock.** An advisory `flock` on `state.json` for the duration of `apply`.
  A second `apply` waits or fails with "another bedouin is running", rather
  than interleaving writes.
- **Atomic write.** Serialize to `state.json.tmp` in the same directory,
  `fsync`, then `rename`. A crash mid-write leaves the previous state intact.
- **Mode 0600.**
- **An unreadable, malformed, or newer-`schema_version` state file is a hard
  error**, never an empty state. Bedouin refuses to run and says how to
  inspect or move the file aside. Silently continuing would re-adopt every
  managed package as `preexisting` and disable uninstall permanently.

A missing state file *is* an empty state — that is a first run, and it is the
only case that means it.

## 11. The Host seam and testing

All I/O in `bedouin-core` goes through one trait:

```rust
pub struct Cmd {
    pub argv: Vec<String>,          // argv, never a shell string
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub root: bool,                 // §8.2
    pub timeout: Option<Duration>,
}

pub trait Host {
    fn run(&self, cmd: &Cmd, out: &mut dyn FnMut(Line)) -> Result<ExitStatus>;
    fn which(&self, bin: &str, path: &[PathBuf]) -> Option<PathBuf>;
    fn read(&self, p: &Path) -> Result<Option<Vec<u8>>>;
    fn write(&self, p: &Path, bytes: &[u8], mode: u32) -> Result<()>;
    fn remove(&self, p: &Path) -> Result<()>;
    fn mkdir_p(&self, p: &Path) -> Result<()>;
    fn symlink_meta(&self, p: &Path) -> Result<Option<Meta>>;   // §9.1
    fn env(&self) -> &BTreeMap<String, String>;
}
```

`run` takes a line callback rather than returning captured output, because
§8.3 streams. `Cmd` carries a timeout because §11's own test list asserts
behavior for "command times out", which revision 1's signature could not
express. `which` takes the search path explicitly — §8.1's whole point is
that Bedouin constructs the PATH rather than inheriting one, and a `which`
that consults the ambient environment would quietly undo that.

Two implementations: `OsHost`, and `FakeHost` — an in-memory filesystem plus
a scripted command table recording invocations and returning canned exit
codes, output, and timeouts.

Three test layers:

1. **Pure unit tests, no Host.** Arm selection, the implication lattice,
   tie detection, `only:` pruning, the deserializer's error messages, path
   normalization, DAG construction and cycle detection, diffing. `resolve()`
   is a pure function of (config, facts) and carries the densest coverage.
2. **`FakeHost` integration tests.** Whole `plan` and `apply` runs against a
   simulated fresh machine, including the failure paths: manager missing,
   command nonzero, command times out, command prints garbage, privilege
   unavailable partway, state file locked, interrupted apply resumed.
   This is what makes the fresh-box path — the one nobody can hand-test
   repeatedly — actually testable.
3. **Docker smoke tests.** One image per distro (`ubuntu:24.04`,
   `opensuse/tumbleweed`) running a real `apply` against a real package
   manager, asserting the binary exists and the rc file is sourced. Slow,
   few, CI-only. macOS uses a CI runner, not a container. These run as root,
   which is why §8.2 needs its `root` case.

Layer 2 is where behavioral confidence lives. Layer 3 exists to catch the
lies in layer 2's fakes.

## 12. Errors

Every parse error carries `file:line:col` and, where a name was involved, a
did-you-mean over the known set. `deny_unknown_fields` is not used directly —
serde's derived unknown-field error cannot carry the rejected-key table — so
each struct implements the check itself, letting `fromEnv`, `fromScript`,
`script`, `exitCode`, and `matcher` produce messages naming their
replacement rather than merely reporting them unknown.

    bedouin.yaml:14:7: unknown arm `mcaos`
       |
    14 |       mcaos: nightly
       |       ^^^^^ did you mean `macos`?
       |
       = built-in arms: macos, linux, ubuntu, … (see --list-arms)
       = declared targets: work, noble

Caret rendering via `annotate-snippets` is a nice-to-have; `file:line:col`
plus the hint suffices for M0.

Three cases that are errors, not surprises: no config file found anywhere in
§4.1's search order; a `version:` other than `0`; and a config that parses to
zero items (which reports "nothing declared" rather than planning an empty
run against a populated state and proposing to remove everything).

## 13. Milestone split

**M0** — crates, schema types, the hand-written `Value<T>` deserializer, the
facts resolver, the seven-stage loader, `only:` pruning, `resolve()`, path
normalization, DAG construction, diffing against state, `plan` output. `Host` exists; `FakeHost` drives every test. `OsHost` gains
only its **read-only** methods in M0 — `plan` genuinely probes the machine,
so a read-only `OsHost` is an M0 deliverable, and the state store gains its
reader. Nothing mutates the machine.

**M1** — `OsHost`'s write and execute paths, the executor for
managers/languages/shell/packages/files/rc blocks/PATH, installer recipes,
privilege handling and the keepalive, state writes with locking and atomic
replacement, `apply` and `apply -f`, failure and resume semantics, docker
smoke tests, musl and universal release builds.

**M1.5** — `init`, `add`, `sync`. **Done.**

**M2** — `doctor`, drift reporting, `remove`, SUSE as a tested execution
target. **Done.**

**M4** — `absorb` to the ceiling stated in §17, `reconcile --watch`, and
`daemon install` generating systemd/launchd units. **Done.**

**M3** — the Tauri app. Deliberately last: the user called it "a luxury, not a
necessity", and the bootstrap path must never need a webview.

Also closed since M1: the plan artifact (`plan -o` / `apply -f`), the state
lock, and the sudo keepalive -- the three items §14a.1 and §8.2 deferred.

## 14. Deferred, with reasons

| Deferred | Until | Why not now |
|---|---|---|
| `sources:` / any plan-time execution | v2 | §6.5 — on a fresh box the script branch is dead code |
| `fromEnv` | never | `{{ env.X \| default(…) }}` |
| Nested conditionals | never | unrepresentable by construction (§6.7) |
| Vars referencing vars | never | keeps resolution two flat layers, not a fixpoint |
| Negation in `only:` | never | declare a target instead |
| `bedouin upgrade` | post-M1 | `latest` deliberately does not auto-upgrade (§7.2) |
| `--keep-going` | post-M1 | needs a failure-summary design |
| Per-manager package aliases | M2 | SUSE support forces the issue |
| `init`, `add`, `sync` | M1.5 | conveniences over a core that must work first |
| `doctor`, drift reporting, `remove` command | M2 | removal as a plan outcome is in M1 |
| Tauri app | M3 | webkit2gtk must stay off the bootstrap path |
| `absorb`, `reconcile --watch` | M4 | |

## 14a. Deltas between this spec and what M0 shipped

Recorded so the document going into M1 is truthful rather than aspirational.

1. **The plan artifact (`plan -o`, §7.3) moved to M1.** It has no consumer
   until `apply -f` exists, and the referenced-env computation is real work
   that belongs beside the thing that reads it. `plan` and `apply` in one
   process are unaffected, because facts are resolved once either way.
2. **Version comparison in the diff (§7.2) is presence-only in M0.** A binary
   found on the search path counts as installed. Comparing versions needs the
   per-manager probe commands of §8.4's recipe table, which lands with the
   executor.
3. **Unknown config keys use serde's derived check, not the hand-rolled one
   §12 describes.** The derived message already lists the expected keys, and
   the rejected-key table (`fromEnv`, `fromScript`, `matcher`, …) lives where
   it actually matters, inside `Value`'s deserializer. Revisit when the
   derived message grates.
4. **`needs:` naming a package that `only:` pruned drops the edge** rather
   than erroring. `zellij needs build-essential` is correct on Linux and
   meaningless on macOS, and one config has to say both. A `needs:` naming a
   package that was never declared is still an error. This rule was missing
   from the spec entirely; the acceptance test found it.
5. **`plan` verifies each `files[].src` exists and is inside the config
   root.** A plan naming a source that is not there is not a faithful
   prediction of apply, and the check is read-only and free.
6. **The built-in arm vocabulary includes `{distro_like}-{arch}` names**
   (§6.1), which the first draft claimed it did not.
7. **Fact values have exactly one spelling.** `distro_like` is `debian` in
   `match:`, in templates, in `bedouin facts`, and in the state file; the arm
   vocabulary separately defines `debian-like` as an *arm name*. The two were
   conflated, so `match: { distro_like: debian }` compared `debian` against
   `debian-like` and silently never matched.
8. **`match:` values are validated against the closed set the resolver can
   produce.** `os: darwin` was not an error, it was a branch that never
   matched on any machine — the failure class §6.1 exists to eliminate, in the
   one place still comparing raw strings.
9. **Empty collections are refused.** `only: []` pruned the item everywhere in
   silence (and proposed uninstalling it if state owned it); `from: []`
   produced a sentence with a hole in it. Both now error, matching the
   existing treatment of `match: {}`.
10. **An `includes:` pattern matching no files is an error.** Expanding to
    nothing silently drops every item the drop-in declares, and anything
    already in state as `owner: bedouin` is then planned for *removal* — a
    one-character typo read as "uninstall all of this".
11. **Item ids are checked for uniqueness across the whole plan**, not only
    per kind. §7.2 mandated it; rc ids (`rc/{package}/{basename}`) collided
    within a single package and the collision was silently deduped.
12. **`facts.user` falls back to `id -un`, then to the home directory's own
    name.** `$USER` is absent under most container runtimes and some CI, and an
    empty `{{ user }}` rendered silently into whatever template used it. Found
    by the SUSE container, which has no `$USER`.
12b. **The diff honours `status: incomplete`** (§8.3). It read presence in the
    state map alone, so a half-installed item planned as a no-op and `plan`
    exited 0 claiming the machine matched the config.

## 14b. Deltas from the M1 executor review

The review found 32 confirmed defects, 16 of them data-loss. What changed:

13. **Bedouin owns rc *blocks*, never rc files.** §9 always said so; the code
    had an `owns_file` flag that made every package `rc:` entry claim its whole
    file, and the executor then upserted into an empty string. A block aimed at
    the user's own `~/.zshrc` replaced it with one bedouin block and no backup,
    and two packages sharing a drop-in file silently lost the first one's block.
    The flag is gone. An emptied drop-in file is tidied only when it sits inside
    `shell.rc_dir`.
14. **The PATH file is one item, not one per entry.** Every `path/{entry}` item
    recorded the same generated file as its own, so dropping one entry deleted
    the whole file while the survivors — all `complete` — never rewrote it.
    §7.4's per-entry lines move to `plan -v`.
15. **The diff is content-addressed for files, rc blocks and the PATH file.**
    It compared ids alone, so the hash the executor recorded was never read
    back and editing a template or a `vars:` value did nothing, forever.
16. **The intent marker flips `status`; it no longer replaces the record.**
    §8.3 asks for a flip. Replacing it discarded `method`, `backup`,
    `owned_files` and `rc_blocks`, so a step that failed left bedouin amnesiac
    about a package it had installed — permanently unowned.
17. **Backups append rather than replace the extension** (`init.lua.bedouin-bak`,
    per §9.1), and an existing backup is never overwritten: a re-adopt was
    saving Bedouin's own render over the user's only copy.
18. **The symlink refusal covers every managed write**, not just `files:`.
    `OsHost::write` renames over the path, which severs a dotfiles-repo symlink.
19. **Non-UTF-8 is refused rather than decoded.** Reads went through
    `from_utf8_lossy` and the result was written straight back, turning every
    stray byte into U+FFFD — in the user's live rc file and in its backup.
20. **A removal whose uninstaller fails drops the record and warns**, rather
    than aborting. Stop-on-first-failure plus drop-only-on-success re-ran the
    same doomed command first on every future apply.
21. **`from: rustup` is a parse error.** It installs toolchains, not packages,
    so it ignored the package name and reported success.
22. **`OsHost::run` drains both pipes concurrently and enforces
    `Cmd.timeout`.** It read stdout to EOF first, so a step filling the stderr
    pipe deadlocked with nothing to break it; the timeout was accepted and
    never armed.

## 16. Aliases and completions

Added after M1 at the user's request. Both are things a dotfiles manager is
expected to do, both are painful to express as raw `rc:` content because the
syntax differs per shell, and both are cheap given the machinery that exists.

### 16.1 Aliases

```yaml
aliases:                    # global
  ll: ls -alh
  g: git

packages:
  - name: kubectl
    from: apt
    aliases:                # scoped to this package
      k: kubectl
      kgp: kubectl get pods
```

Per-package aliases render into **that package's own rc block**, alongside
whatever `rc:` content it already declares. Global aliases become one
Bedouin-owned block, `rc/bedouin/aliases`.

They are deliberately *not* merged into a single shared aliases file. The PATH
file is one artifact because PATH entries are ordered fragments of one
variable; aliases are independent, and a package's aliases belong with the
package so that dropping it removes them through machinery that already exists
and already converges. A shared file would reintroduce exactly the
shared-artifact coupling §14b.13 and §14b.14 were written to remove.

Rendering is per shell, because the syntax genuinely differs:

| Shell | Line |
|---|---|
| zsh, bash | `alias k='kubectl'` |
| fish | `alias k 'kubectl'` |

Values are single-quoted, and an embedded `'` is escaped (`'\''` for
posix shells, `\'` for fish). Alias values are user text landing in a file the
shell evaluates, so the quoting rule is load-bearing rather than cosmetic.

### 16.2 Completions

```yaml
packages:
  - name: kubectl
    from: apt
    completions:
      generate: ["kubectl", "completion", "{{ shell.name }}"]
```

`generate` is argv, run **at apply time**, after its own package is installed,
and its stdout is written to the shell's completions directory. Nothing
evaluates it.

**This is not a hole in §6.5.** That rule forbids user-supplied code that
determines the *plan*, and the argument that killed `fromScript` was ordering:
on a fresh box a plan-time script runs before Bedouin has installed anything,
so its fallback is always the real value. Neither applies here. This runs
during apply, after the tool it invokes exists — the package is a hard
ordering dependency — and it produces file *content*, the same category as
rendering a template. Two guards keep it a boundary rather than a leak: it
goes through the same argv-only `Cmd` path as every other step, so no shell
ever sees it; and its output is written, never executed.

Completion output lands in a file Bedouin wholly owns:

| Shell | Path | Wiring |
|---|---|---|
| zsh | `{rc_dir}/completions/_{name}` | dir added to `fpath` in the source block |
| bash | `{rc_dir}/completions/{name}.bash` | sourced from the source block |
| fish | `~/.config/fish/completions/{name}.fish` | native |

Those bytes are tool output, not managed text: no sentinels, no UTF-8
refusal, and the file is recorded in `owned_files` so removal deletes it.

**Drift coverage is partial, and says so.** The item is content-addressed on
the *generate command*, so editing `generate:` re-runs it; a package
`Upgrade` or `Reinstall` also re-runs it, since output can differ by version.
Whether the *output* changed cannot be known at plan time without running the
command, which plan does not do. `doctor` therefore reports a hand-edited
completions file as drift, but cannot report a completion that is merely
stale.

## 17. `absorb`, and the ceiling it stops at

The handoff describes absorb as a three-way merge: original render versus
current file versus new render, with marked-region edits mapping back to their
config entry. That is a merge engine. What ships is the useful half of it,
and the cut is stated here rather than discovered later.

**What absorb does.** `doctor` already finds drift by comparing recorded
hashes against the machine. `absorb` walks those findings and, for each,
offers to lift the edit back into the config:

- **An rc block** — the edit is between markers Bedouin owns, so the new
  content is exactly the block's current text. Absorb rewrites that entry's
  `content:` in the config. This is the common case and the one the handoff
  cares about: you tweak an alias in your shell, and absorb puts the tweak in
  the file that survives the machine.
- **A managed file whose `src:` contains no template syntax** — the template
  is a plain copy, so the edited file *is* the new template. Absorb copies it
  back.
- **A managed file whose `src:` is templated** — refused, with a diff. Mapping
  a rendered edit back through minijinja is inverting a template, and there is
  no honest way to guess which part of the output came from which expression.
  The `render_snapshot` in state is what makes even the diff possible.

**Why this is the right cut.** The genuinely hard case — a templated file
edited in a region that a loop or conditional produced — is rare, and getting
it wrong writes nonsense into the config the user trusts. Refusing loudly and
showing the diff leaves them better off than a merge that guesses.

**Absorb never edits blind.** It shows what it will write and asks, and every
config edit goes through the same reparse-verify as `add` and `remove` (§14a
pattern): if the result would not parse, it refuses and says to edit by hand.

## 18. `bedouin env` and `.env.bedouin`

A config reads environment variables — `{{ env.X }}`, `match: { env: … }` — and
nothing told you which. §7.3 already scans the raw config for exactly that set
to freeze into a plan artifact; this exposes the same scan.

```console
$ bedouin env
Variables this config reads:

  BEDOUIN_PROFILE   set      targets.work
  GIT_USER_EMAIL    not set  files[0].src (templates/gitconfig.j2)
  ZELLIJ_VERSION    not set  packages[zellij].version   (has a default)

2 of 3 are unset. 1 of those has no default and will fail to resolve.
```

**Names and set/unset only, never values** — same rule as `bedouin facts`, and
for the same reason: this output ends up in bug reports.

### `.env.bedouin` is read, not merely written

`bedouin env --write` scaffolds a commented file beside the config:

```sh
# Variables bedouin.yaml reads. Values here are loaded before facts resolve.
# NOT for secrets you would not want beside your config -- see below.
# BEDOUIN_PROFILE=
GIT_USER_EMAIL=
```

**Bedouin loads this file if it exists**, before resolving facts, with the
process environment winning on a collision. A file the tool writes and never
reads is a trap: the user fills it in, nothing consumes it, and the confusion
is silent. Giving it a reader is what makes the scaffold honest.

Consequences, stated because they are not free:

- It goes in `.gitignore`. `--write` adds it if a `.gitignore` is present, and
  says so.
- Bedouin warns if it is group- or world-readable.
- Its values reach the plan artifact like any other referenced variable, at
  mode 0600 (§7.3).
- For real secrets, keep using the shell — `01-secrets.zsh` reading `op://`
  references stays the better pattern, and this does not replace it.

`plan` also warns when a referenced variable is unset **and** has no
`| default(…)`, since that is a resolve-time failure waiting to happen.

## 21. Editing the config without opening it

`add` and `remove` already existed. The rest of the config deserves the same
treatment: discovering that a tool you already have ships a completion
generator should not mean opening a file to write four lines.

```console
$ bedouin alias gs='git status'                 # global
$ bedouin alias z=zellij --package zellij       # scoped to a package
$ bedouin completions kubectl -- kubectl completion '{{ shell.name }}'
$ bedouin add cargo:zellij@0.40.1 \
    --alias z=zellij --alias za='zellij attach' \
    --path '{{ home }}/.cargo/bin' \
    --completions 'zellij setup --dump-completion {{ shell.name }}'
```

Everything the same way it already worked, because that part was hard-won:

- **Text surgery**, so comments and ordering survive.
- **A structural guard** — the parsed result must equal the parsed original
  plus exactly this change, the rule `remove` got after §14b.
- **Write, verify, roll back** — the edit is written, the config is re-loaded,
  and a config bedouin can no longer read is restored untouched.
- `add`'s extras apply to one text before any of it is written, so a bad
  `--alias` leaves nothing half-added.

Two details that are not obvious:

**Alias values are quoted going in.** Shell is full of `:` and `#`, and a bare
YAML scalar containing either means something else.

**`--completions` splits on whitespace, except inside `{{ … }}`.** A naive
split turns `{{ shell.name }}` into three arguments and the template stops
being one. Anything with real shell syntax should use the `--` form, which
needs no guessing at all.

## 15. Departures from the handoff

The handoff is approved; these are the places this spec knowingly differs.
Everything else it does not contradict remains in force.

1. **`targets:` gains `name:`** and its entries double as arm names (§6.2).
   The handoff's `targets:` had no names because nothing referenced them.
2. **Target resolution is stated, not changed.** The handoff says first match
   wins; §6.3 rule 2 keeps that for declared targets, and adds a rule for
   built-in arms, which the handoff did not have.
3. **A top-level `shell:` declaration** is added (§3.1). The handoff treated
   shell purely as a fact, which breaks on the fresh box where Bedouin is
   installing the shell.
4. **`only:` and evaluatable `package_managers:`** are additions (§6.6),
   required for one config to cover Ubuntu and macOS.
5. **`needs:`** is an addition (§7.1) for build prerequisites the DAG cannot
   infer.
6. **`version: latest` does not auto-upgrade** (§7.2). The handoff wrote
   `version: latest` without defining it.
7. **`init`, `add`, `sync` move to M1.5** (§1). The handoff lists them in v1
   without assigning a milestone.
8. **`aliases:` and `completions:`** are additions (§16), requested after M1.
   Both are expected of a dotfiles manager, both are awkward as raw `rc:`
   content because the syntax is shell-specific, and both are cheap given the
   rc-block machinery that already exists.
