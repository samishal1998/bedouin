//! The M1 acceptance test: `plan -> apply -> plan` on a simulated fresh box.
//!
//! The second plan exiting 0 is the whole bar. It means the executor did what
//! the plan predicted, recorded it truthfully, and that a re-run is a no-op --
//! which is idempotence, resume, and diff correctness in one assertion.

use bedouin_core::apply;
use bedouin_core::facts::{Arch, Os};
use bedouin_core::host::{FakeHost, FakeRun, Host, Line};
use bedouin_core::plan::Action;
use bedouin_core::run;
use std::path::Path;

const CONFIG: &str = r#"
version: 0
shell: zsh

vars:
  editor: nvim

package_managers: [apt]

languages:
  - name: rust
    version: "1.80"
    installer: rustup

packages:
  - name: jq
    from: apt

  - name: zellij
    from: cargo
    path: ["{{ home }}/.cargo/bin"]
    rc:
      - file: "{{ shell.rc_dir }}/70-zellij.zsh"
        content: |
          eval "$(zellij setup --generate-auto-start zsh)"

files:
  - src: templates/gitconfig.j2
    dest: ~/.gitconfig
"#;

/// A fresh Ubuntu: apt present, nothing else, sudo needs no password.
fn fresh() -> FakeHost {
    FakeHost::new()
        .with_file("/cfg/bedouin.yaml", CONFIG)
        .with_file(
            "/cfg/templates/gitconfig.j2",
            "[core]\n\teditor = {{ vars.editor }}\n",
        )
        .with_file(
            "/etc/os-release",
            "ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n",
        )
        .with_env("HOME", "/home/t")
        .with_env("USER", "t")
        .with_env("SHELL", "/bin/bash")
        .with_env("PATH", "/usr/bin:/bin")
        .with_binary("/usr/bin/apt-get")
        .with_command("id -u", FakeRun::ok("1000"))
        .with_command("sudo -n true", FakeRun::ok(""))
        // What the executor will actually run.
        .with_command("curl --proto =https --tlsv1.2 -sSfL https://sh.rustup.rs -o /tmp/bedouin-rustup.sh", FakeRun::ok(""))
        .with_command("sh /tmp/bedouin-rustup.sh -y --no-modify-path", FakeRun::ok("rustup installed"))
        .with_command("rustup toolchain install 1.80", FakeRun::ok("toolchain 1.80 installed"))
        .with_command("sudo -n apt-get update", FakeRun::ok("Reading package lists"))
        .with_command("sudo -n apt-get install -y jq", FakeRun::ok("Setting up jq"))
        .with_command("cargo install --locked zellij", FakeRun::ok("Installed zellij"))
}

fn plan_on(h: &FakeHost) -> run::Outcome {
    run::plan_for(
        h,
        Some(Path::new("/cfg/bedouin.yaml")),
        Path::new("/cfg"),
        Os::Linux,
        Arch::X86_64,
    )
    .unwrap_or_else(|e| panic!("plan failed: {e}"))
}

fn apply_on(h: &FakeHost) -> apply::Report {
    let o = plan_on(h);
    apply::apply(&o.plan, &o.config, &o.facts, o.state, h, &mut |_: Line| {})
        .unwrap_or_else(|e| panic!("apply failed: {e}"))
}

fn read(h: &FakeHost, p: &str) -> Option<String> {
    h.read(Path::new(p))
        .unwrap()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

#[test]
fn plan_apply_plan_converges() {
    let h = fresh();

    let before = plan_on(&h);
    assert_eq!(before.plan.exit_code(), 2, "a fresh box has work to do");

    let report = apply_on(&h);
    assert!(report.ok(), "apply failed: {:?}", report.failure);
    assert!(!report.completed.is_empty());

    // The bar: nothing left to do.
    let after = plan_on(&h);
    assert_eq!(
        after.plan.exit_code(),
        0,
        "second plan still wants changes:\n{}",
        after.plan.render(true)
    );
    assert!(after.plan.items.iter().all(|i| i.action == Action::NoOp));

    // And a second apply is a no-op rather than a repeat.
    let again = apply_on(&h);
    assert!(again.completed.is_empty(), "a second apply must do nothing");
}

#[test]
fn the_toolchain_is_installed_before_the_package_that_needs_it() {
    // The Ansible reload problem: installing rust and then a cargo package in
    // one run only works if the second step's PATH knows where the first put
    // cargo.
    let h = fresh();
    apply_on(&h);
    let ran: Vec<String> = h.ran.borrow().iter().map(|c| c.display()).collect();
    let pos = |needle: &str| ran.iter().position(|c| c.contains(needle));
    assert!(pos("rustup toolchain install").unwrap() < pos("cargo install").unwrap());

    // The cargo step's own PATH carries the directory rustup's recipe records.
    let cargo_step = h
        .ran
        .borrow()
        .iter()
        .find(|c| c.display().contains("cargo install"))
        .cloned()
        .expect("the cargo step ran");
    assert!(
        cargo_step.env["PATH"].contains("/home/t/.cargo/bin"),
        "PATH was {}",
        cargo_step.env["PATH"]
    );
    // Never the parent shell's environment.
    assert!(!cargo_step.env["PATH"].contains("/usr/local/sbin"));
}

#[test]
fn only_the_steps_that_need_root_are_escalated() {
    let h = fresh();
    apply_on(&h);
    let ran: Vec<String> = h.ran.borrow().iter().map(|c| c.display()).collect();
    assert!(ran.iter().any(|c| c == "sudo -n apt-get install -y jq"));
    assert!(
        ran.iter().any(|c| c == "cargo install --locked zellij"),
        "a per-user manager must not be run as root: {ran:?}"
    );
    assert!(!ran.iter().any(|c| c.starts_with("sudo") && c.contains("cargo")));
}

#[test]
fn what_was_written_is_what_the_config_asked_for() {
    let h = fresh();
    apply_on(&h);

    let git = read(&h, "/home/t/.gitconfig").expect("the managed file");
    assert!(git.contains("editor = nvim"), "template rendered: {git}");

    let rc = read(&h, "/home/t/.zshrc.d/70-zellij.zsh").expect("the drop-in");
    assert!(rc.contains("zellij setup"));
    assert!(rc.contains("# >>> bedouin: zellij >>>"), "sentinels present");

    let path = read(&h, "/home/t/.zshrc.d/00-bedouin-path.zsh").expect("the PATH file");
    assert!(path.contains("export PATH=\"/home/t/.cargo/bin:$PATH\""));

    // The block that makes the rc file read the drop-in directory.
    let zshrc = read(&h, "/home/t/.zshrc").expect("the rc file");
    assert!(zshrc.contains("# >>> bedouin: source >>>"));
    assert!(zshrc.contains("/home/t/.zshrc.d"));
}

#[test]
fn state_records_ownership_versions_and_bin_dirs() {
    let h = fresh();
    apply_on(&h);
    let state = read(&h, "/home/t/.local/state/bedouin/state.json").expect("state written");
    let v: serde_json::Value = serde_json::from_str(&state).unwrap();
    let items = &v["items"];

    assert_eq!(items["package/jq"]["owner"], "bedouin");
    assert_eq!(items["package/jq"]["status"], "complete");
    assert_eq!(items["package/jq"]["method"], "apt");
    assert_eq!(items["language/rust"]["version"], "1.80");
    // The bin directory the step environment is assembled from.
    assert_eq!(
        items["language/rust"]["bin_dirs"][0],
        "/home/t/.cargo/bin",
        "without this the cargo step cannot find cargo"
    );
    // The snapshot M4's three-way absorb needs; unreconstructible later.
    assert!(items["file//home/t/.gitconfig"]["render_snapshot"].is_string());
}

#[test]
fn a_failed_step_stops_the_run_and_says_what_did_not_happen() {
    let h = fresh().with_command(
        "sudo -n apt-get install -y jq",
        FakeRun::fails(100, "E: Unable to locate package jq"),
    );
    let o = plan_on(&h);
    let report = apply::apply(&o.plan, &o.config, &o.facts, o.state, &h, &mut |_| {}).unwrap();

    assert!(!report.ok());
    let f = report.failure.as_ref().unwrap();
    assert_eq!(f.id, "package/jq");
    assert!(f.message.contains("exited 100"), "{}", f.message);
    assert!(
        f.output_tail.iter().any(|l| l.contains("Unable to locate")),
        "the tail of what it printed: {:?}",
        f.output_tail
    );
    assert!(!report.not_attempted.is_empty(), "later steps are named");

    let text = report.render();
    assert!(text.contains("Not attempted:"), "{text}");
    assert!(text.contains("re-run"), "says how to recover: {text}");
}

#[test]
fn an_interrupted_step_is_recorded_before_it_runs() {
    // Recording only on success leaves a package bedouin installed looking
    // pre-existing -- permanently un-removable, silently. So the record goes in
    // first, and a failure leaves `incomplete` behind rather than nothing.
    let h = fresh().with_command(
        "sudo -n apt-get install -y jq",
        FakeRun::fails(1, "boom"),
    );
    apply::apply(
        &plan_on(&h).plan,
        &plan_on(&h).config,
        &plan_on(&h).facts,
        plan_on(&h).state,
        &h,
        &mut |_| {},
    )
    .unwrap();

    let state = read(&h, "/home/t/.local/state/bedouin/state.json").unwrap();
    let v: serde_json::Value = serde_json::from_str(&state).unwrap();
    assert_eq!(v["items"]["package/jq"]["status"], "incomplete");
    assert_eq!(v["items"]["package/jq"]["owner"], "bedouin");

    // ...and the next plan therefore wants to do it again.
    let next = plan_on(&h);
    let jq = next.plan.items.iter().find(|i| i.name == "jq").unwrap();
    assert_ne!(jq.action, Action::NoOp);
}

#[test]
fn a_run_that_cannot_escalate_refuses_to_start() {
    // Failing at step nineteen of twenty because sudo is unavailable is worse
    // than never beginning.
    let h = fresh()
        .with_command("sudo -n true", FakeRun::fails(1, "no"))
        .with_command("id -nG", FakeRun::ok("t users"));
    let o = plan_on(&h);
    let e = apply::apply(&o.plan, &o.config, &o.facts, o.state, &h, &mut |_| {})
        .unwrap_err()
        .to_string();
    assert!(e.contains("need root"), "{e}");
    assert!(e.contains("package/jq"), "names the steps: {e}");
    // Nothing ran beyond the read-only fact probes.
    assert!(!h.ran.borrow().iter().any(|c| c.display().contains("apt-get")));
}

#[test]
fn an_existing_file_is_backed_up_before_it_is_overwritten() {
    // §9.1: packages had a `preexisting` protection and files had none, so a
    // first apply used to destroy the user's own ~/.gitconfig.
    let h = fresh().with_file("/home/t/.gitconfig", "[user]\n\tname = mine\n");
    let before = plan_on(&h);
    let git = before.plan.items.iter().find(|i| i.name == "~/.gitconfig").unwrap();
    assert_eq!(git.action, Action::Adopt, "an existing file is adopted, not created");

    apply_on(&h);
    let backup = read(&h, "/home/t/.gitconfig.bedouin-bak").expect("the original is kept");
    assert!(backup.contains("name = mine"));
    assert!(read(&h, "/home/t/.gitconfig").unwrap().contains("editor = nvim"));
}

#[test]
fn bedouin_refuses_to_write_through_a_symlink() {
    // Writing through one puts content somewhere the config does not name.
    let h = fresh();
    h.symlinks
        .borrow_mut()
        .insert("/home/t/.gitconfig".into(), "/etc/passwd".into());
    let o = plan_on(&h);
    let report = apply::apply(&o.plan, &o.config, &o.facts, o.state, &h, &mut |_| {}).unwrap();
    let f = report.failure.as_ref().expect("must refuse");
    assert!(f.message.contains("symlink"), "{}", f.message);
}

/// The same fresh machine, but the config no longer declares the shell files.
fn without_shell_files(h: FakeHost) -> FakeHost {
    let stripped = CONFIG
        .replace("    path: [\"{{ home }}/.cargo/bin\"]\n", "")
        .replace(
            "    rc:\n      - file: \"{{ shell.rc_dir }}/70-zellij.zsh\"\n        content: |\n          eval \"$(zellij setup --generate-auto-start zsh)\"\n",
            "",
        )
        .replace("files:\n  - src: templates/gitconfig.j2\n    dest: ~/.gitconfig\n", "");
    h.with_file("/cfg/bedouin.yaml", &stripped)
}

#[test]
fn dropping_items_from_the_config_actually_removes_them() {
    let h = fresh();
    apply_on(&h);
    assert!(read(&h, "/home/t/.zshrc.d/70-zellij.zsh").is_some());
    assert!(read(&h, "/home/t/.zshrc.d/00-bedouin-path.zsh").is_some());

    let h = without_shell_files(h);
    let report = apply_on(&h);
    assert!(report.ok(), "{:?}", report.failure);

    // Files bedouin created outright are gone, not merely unlisted.
    assert!(read(&h, "/home/t/.zshrc.d/70-zellij.zsh").is_none());
    assert!(read(&h, "/home/t/.zshrc.d/00-bedouin-path.zsh").is_none());
    assert!(read(&h, "/home/t/.gitconfig").is_none());

    // And the run converges.
    assert_eq!(plan_on(&h).plan.exit_code(), 0);
}

#[test]
fn removing_a_managed_file_gives_the_user_theirs_back() {
    // The failure this guards is data loss: the restore used to read the plan
    // payload, which a removal does not have, so the managed file was deleted
    // and the backup kept -- the user's own content gone in all but name.
    let h = fresh().with_file("/home/t/.gitconfig", "[user]\n\tname = mine\n");
    apply_on(&h);
    assert!(read(&h, "/home/t/.gitconfig").unwrap().contains("editor = nvim"));
    assert!(read(&h, "/home/t/.gitconfig.bedouin-bak").is_some());

    let h = without_shell_files(h);
    apply_on(&h);
    assert_eq!(
        read(&h, "/home/t/.gitconfig").as_deref(),
        Some("[user]\n\tname = mine\n"),
        "the file the user had before bedouin touched it"
    );
    assert!(
        read(&h, "/home/t/.gitconfig.bedouin-bak").is_none(),
        "and no stray backup left behind"
    );
}

#[test]
fn removing_a_block_leaves_the_rest_of_the_users_rc_file_alone() {
    let h = fresh().with_file("/home/t/.zshrc", "export EDITOR=vi\nalias ll='ls -l'\n");
    apply_on(&h);
    assert!(read(&h, "/home/t/.zshrc").unwrap().contains("bedouin: source"));

    let h = without_shell_files(h);
    apply_on(&h);
    let rc = read(&h, "/home/t/.zshrc").unwrap();
    assert_eq!(rc, "export EDITOR=vi\nalias ll='ls -l'\n", "restored exactly");
}

#[test]
fn a_package_that_was_already_on_the_machine_is_never_removed() {
    // §10: adoption is what makes uninstall safe. A jq that predates bedouin
    // must survive being dropped from the config.
    let h = fresh().with_binary("/usr/bin/jq");
    apply_on(&h);
    let state = read(&h, "/home/t/.local/state/bedouin/state.json").unwrap();
    let v: serde_json::Value = serde_json::from_str(&state).unwrap();
    assert_eq!(v["items"]["package/jq"]["owner"], "preexisting");

    let h = without_shell_files(h).with_file(
        "/cfg/bedouin.yaml",
        "version: 0\nshell: zsh\npackages: [{name: zellij, from: cargo}]\n",
    );
    let o = plan_on(&h);
    assert!(
        !o.plan.items.iter().any(|i| i.name == "jq" && i.action == Action::Remove),
        "a pre-existing package must not be planned for removal"
    );
}
