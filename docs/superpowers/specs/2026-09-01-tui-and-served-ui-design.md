# A TUI, and the surface a web UI and Tauri will reuse

Status: implemented in 0.5.0. Two sections diverged from the design; both are
corrected below and marked.
Supersedes nothing. Extends `2026-08-30-bedouin-m0-m1-design.md` §13 (M3).

## 1. What changed since M0/M1

The original spec parked M3 as "the Tauri app", on the grounds that the user
called it a luxury and that webkit2gtk must stay off the bootstrap path. Both
still hold. What changed is the shape: the first UI is a **web UI served by the
CLI**, not a desktop app, with a **TUI** ahead of it — and Tauri arrives later
as a third consumer of the same surface rather than as the only one.

That reordering is not cosmetic. A served UI keeps webkit2gtk out of the
picture entirely, and a TUI keeps it out *and* needs no new network
dependency. The order below is chosen so each slice is useful alone and none
forecloses the next.

## 2. The finding that shapes everything

`bedouin-core` is already presentation-free. There is no `println!`, no
`eprintln!` and no `stdin` anywhere in it; every entry point takes
`host: &dyn Host` and returns a value; and `plan` and `apply` are already two
separate calls with user approval between them. A TUI, an HTTP handler and a
Tauri command can all drive the identical path today.

So this is not an extraction. What blocks a second consumer is narrow and
concrete:

| Blocker | Where | Cost |
|---|---|---|
| `Plan`, `Item`, `Action`, `Payload` not `Serialize` | plan.rs:32-164 | derive |
| `apply::{Report, Failure}` not `Serialize` | apply.rs:33-50 | derive |
| `doctor::{Report, Drift}` not `Serialize` | doctor.rs:21-94 | derive |
| `Line` carries the step index inside a formatted string | apply.rs:849 | new variants |
| No step-*end* event: success and failure are both silence | apply.rs:887-925 | new variant |
| `Host` is not `Send` | host.rs:120 | one bound |

Roughly 70% of the read model a UI needs — `Facts`, resolved `Config`,
`State`, `Artifact` — already round-trips through serde, because the plan
artifact forced the issue in M1. This work finishes the other 30%.

## 3. Decisions

**Scope: observe, then apply with confirmation.** The UI shows plan, drift and
environment, and can trigger an apply behind an explicit confirmation. It does
not edit the config. Config editing (`add`, `alias`, `absorb`) stays a terminal
concern for now; §8 says why.

**Order: enabling changes + TUI, then web UI, then Tauri.** The TUI is the
cheapest real consumer — no network dependency, no asset embedding, and it has
a terminal, so it sidesteps the sudo problem in §6 that the web UI cannot.
Proving the surface against one real consumer before adding a second is the
point of the ordering.

**No session module, no new crate.** The alternative — moving the ~700 lines of
policy that live in `bedouin-cli/src/main.rs` (the lock/keepalive wrapper,
`edit_then_apply`, sync, absorb, add/alias parsing) into a shared
`bedouin-core::session` — is real work that buys nothing until a second
consumer needs those specific commands. The TUI needs plan, apply, doctor and
env, all of which are already callable. Policy moves when a consumer asks for
it, not before.

**`Serialize` only, not `Deserialize`.** Nothing reads a plan back: `apply -f`
goes through `Artifact`, which is already both directions. Deriving only what
is needed avoids committing to a stable wire format before there is a consumer
to keep stable for. When the web UI lands, the plan JSON gets the same
treatment `Artifact` has — a version field and a compatibility check.

## 4. `Host: Send` — NOT DONE, and not needed

*Corrected after implementation.* The reasoning below was right about `Sync`
and wrong about `Send`: it justified `Send` by "the worker thread the TUI runs
apply on", and §6 no longer has a worker thread. Nothing in the shipped TUI
crosses a thread boundary, so the bound was added and then reverted. It is a
one-line change whenever the web UI needs it.

The `Sync` analysis stands as written and still applies.

## 4a. The original reasoning

An early reading of this work claimed `Send + Sync` was a one-line change. It
is not. `FakeHost` holds three `RefCell` fields (host.rs:421-428), and
`RefCell` is `Send` but not `Sync`, so adding `Sync` breaks every test that
uses the fake — which is all of them.

`Send` alone is free and sufficient here: `OsHost` holds only a map, `RefCell`
is `Send`, and `Send` is what lets an owned `Box<dyn Host>` move onto the
worker thread the TUI runs `apply` on.

`Sync` is what axum's shared state and `async` Tauri commands need, and it
costs a `RefCell` → `Mutex` conversion across `FakeHost`. That belongs in the
web UI slice, where the requirement is real. Doing it now would be paying a
cost for a consumer that does not exist, and the cost does not grow by waiting.

## 5. `Line` becomes structured

Today the entire step-boundary protocol is one line:

```rust
(ex.out)(Line::Section(format!("[{}/{}] {}", i + 1, changes.len(), item.id)));
```

emitted at exactly one site, with nothing at all emitted when a step ends. Two
consequences: a progress bar must parse `[3/47]` back out of a rendered
string, and **silence is ambiguous** — a step that succeeded, a step that
failed, and a step still running all look identical to a watching UI. That is
the one failure mode a progress display cannot tolerate.

```rust
pub enum Line {
    Out(String),
    Err(String),
    Step    { index: usize, total: usize, id: String },  // replaces Section
    StepEnd { id: String, ok: bool },                    // new
}
```

`Section` is removed rather than kept alongside `Step`. Two spellings of one
boundary would mean every consumer forever has to decide which to honour. It
has two consumers — the CLI's `print_line`, and tests — and both are updated
in the same change. `Line` also gains `Serialize`, which the web UI needs and
which costs nothing now.

The emission contract, which is what every future UI depends on: exactly one
`Step` before each step, exactly one `StepEnd` after it, `ok` false when the
step failed. A test asserts the pairing and ordering over `FakeHost`.

## 6. The TUI — SIMPLIFIED

*Corrected after implementation.* Two changes, both smaller than designed.

**Applying suspends the terminal instead of rendering progress.** The design
had a worker thread streaming `Line` events into a widget, plus a dance of
leaving and re-entering the alternate screen so sudo could prompt. Leaving the
alternate screen for the *whole* apply gets sudo right for the same reason,
and then the run is literally `bedouin apply` — same function, same renderer,
same colours — so the widget, the thread, the channel and the `Send` bound all
stopped being needed. Verified end to end in a container: draw, `a`, `y`,
suspend, real apt install, back to the plan, "No changes."

**One view, not four.** Plan only. Doctor and Env are the same read model with
different fields; they would not have taught us anything about whether the
surface is right, which is what a first consumer is for. They are an
afternoon each if wanted.

## 6a. The original four-view design

`bedouin tui`, behind a cargo feature:

```toml
[features]
default = ["tui"]
tui = ["ratatui", "crossterm"]
```

ratatui and crossterm are pure Rust with no C dependencies, so they link into
the musl target unchanged. Default on, so the released binary has it; the
feature exists so a minimal build is possible, and so the size cost is
measurable rather than assumed. The actual binary delta is reported when the
work lands — the current release is 1.9 MB compressed, and that number is the
thing to protect.

Four views over the read model that already exists: **Plan** (default),
**Doctor**, **Env**, **Apply**.

Apply runs on a worker thread that owns its own `OsHost`; `Line` events flow
back over an `mpsc` channel into the draw loop. It takes `StateLock` exactly as
`run_apply` does (main.rs:163), because `apply` deliberately does not take the
lock itself (apply.rs:808-810) — a second consumer that forgot would corrupt
state, so this is stated rather than assumed. It never applies on launch: plan
first, an explicit key, then a confirmation.

**The sudo problem.** If any step needs root and privilege is `Password`,
`apply` runs `sudo -v` with *inherited stdin* (apply.rs:818). Inside a
raw-mode alternate screen that prompt is invisible and the TUI hangs with no
indication why. So before starting an apply the TUI leaves the alternate
screen, restores cooked mode, lets sudo prompt normally, and re-enters
afterwards. It is cheap, and it is invisible until it breaks.

This is also the reason the web UI is second: it has no terminal to fall back
to, and needs a real answer — `SUDO_ASKPASS`, a privileged helper, or refusing
root steps outright. That decision is deferred to its own design.

## 7. What this must not foreclose

- Every type the TUI consumes is `Serialize`, so a Tauri `#[tauri::command]`
  or an HTTP handler returns the same types with no further core change.
- `Line` maps directly onto `app.emit` (Tauri) or an SSE frame (HTTP).
- `bedouin-app` must be **`exclude`d** from the workspace, not merely left out
  of `default-members`: CI builds all default members with
  `--target x86_64-unknown-linux-musl` (ci.yml:46, and release.yml's target
  matrix at :25-27), so omission alone would not keep webkit2gtk off the
  bootstrap build.
- `run::Outcome` is not serializable and is not made so. Tauri holds it in
  `tauri::State` for free; an HTTP adapter needs a keyed session or must route
  apply through the existing plan artifact. Not solved here, but the surface
  does not assume statelessness either way.

## 8. Deferred, with reasons

| Deferred | Until | Why not now |
|---|---|---|
| Web UI (`bedouin ui`) | next slice | needs the sudo answer in §6 and the first network dependency this project has had |
| `Host: Sync` | web UI slice | costs a `FakeHost` `RefCell`→`Mutex` conversion; no consumer needs it yet |
| `bedouin-core::session` | when a second consumer needs CLI policy | the TUI needs none of it |
| Config editing in a UI | unscheduled | `RawConfig` is `Deserialize`-only and `Value<T>` derives neither, so there is no serializable model of an unresolved config; editing stays text plus `edit.rs` |
| Observable `reconcile --watch` | with the web UI | the loop lives in a clap match arm and is unobservable; a served UI is the first thing that would want to watch it |
| Tauri (`bedouin-app`) | after the web UI | unchanged from M0/M1 §13 |

## 9. Testing

- Serde round-trip tests for each newly-derived type.
- An event-sequence test over `FakeHost`: `Step`/`StepEnd` pair up, in order,
  with `ok` false on the failing step. This is the contract §5 introduces and
  the one every future UI depends on.
- TUI rendering through ratatui's `TestBackend` into a buffer, asserted on
  directly — no terminal needed in CI.
- The existing 230 tests keep passing; the `Line` change touches two consumers
  and both are updated in the same commit.

## 10. Prerequisite, already done

`apply` discarded its `Report` whenever a state write failed (`ex.flush()?`),
so the caller could not say which steps had run — at the one moment that
matters most, since the record of those steps is exactly what was lost. A UI
streaming progress would have blanked what it had already shown. Fixed and
released as 0.4.1 before this work starts, so the TUI builds on a base where
progress survives a failed write.
