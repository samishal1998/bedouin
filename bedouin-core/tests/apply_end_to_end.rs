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
        .with_command(
            "curl --proto =https --tlsv1.2 -sSfL https://sh.rustup.rs -o /tmp/bedouin-rustup.sh",
            FakeRun::ok(""),
        )
        .with_command(
            "sh /tmp/bedouin-rustup.sh -y --no-modify-path",
            FakeRun::ok("rustup installed"),
        )
        .with_command(
            "rustup toolchain install 1.80",
            FakeRun::ok("toolchain 1.80 installed"),
        )
        .with_command(
            "sudo -n apt-get update",
            FakeRun::ok("Reading package lists"),
        )
        .with_command(
            "sudo -n apt-get install -y jq",
            FakeRun::ok("Setting up jq"),
        )
        .with_command(
            "cargo install --locked zellij",
            FakeRun::ok("Installed zellij"),
        )
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
    apply::apply(
        &o.plan,
        &o.config,
        &o.facts,
        o.state,
        h,
        &Default::default(),
        &mut |_: Line| {},
    )
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
    assert!(!ran
        .iter()
        .any(|c| c.starts_with("sudo") && c.contains("cargo")));
}

#[test]
fn what_was_written_is_what_the_config_asked_for() {
    let h = fresh();
    apply_on(&h);

    let git = read(&h, "/home/t/.gitconfig").expect("the managed file");
    assert!(git.contains("editor = nvim"), "template rendered: {git}");

    let rc = read(&h, "/home/t/.zshrc.d/70-zellij.zsh").expect("the drop-in");
    assert!(rc.contains("zellij setup"));
    assert!(
        rc.contains("# >>> bedouin: zellij >>>"),
        "sentinels present"
    );

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
        items["language/rust"]["bin_dirs"][0], "/home/t/.cargo/bin",
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
    let report = apply::apply(
        &o.plan,
        &o.config,
        &o.facts,
        o.state,
        &h,
        &Default::default(),
        &mut |_| {},
    )
    .unwrap();

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
    let h = fresh().with_command("sudo -n apt-get install -y jq", FakeRun::fails(1, "boom"));
    apply::apply(
        &plan_on(&h).plan,
        &plan_on(&h).config,
        &plan_on(&h).facts,
        plan_on(&h).state,
        &h,
        &Default::default(),
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
    let e = apply::apply(
        &o.plan,
        &o.config,
        &o.facts,
        o.state,
        &h,
        &Default::default(),
        &mut |_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("need root"), "{e}");
    assert!(e.contains("package/jq"), "names the steps: {e}");
    // Nothing ran beyond the read-only fact probes.
    assert!(!h
        .ran
        .borrow()
        .iter()
        .any(|c| c.display().contains("apt-get")));
}

#[test]
fn an_existing_file_is_backed_up_before_it_is_overwritten() {
    // §9.1: packages had a `preexisting` protection and files had none, so a
    // first apply used to destroy the user's own ~/.gitconfig.
    let h = fresh().with_file("/home/t/.gitconfig", "[user]\n\tname = mine\n");
    let before = plan_on(&h);
    let git = before
        .plan
        .items
        .iter()
        .find(|i| i.name == "~/.gitconfig")
        .unwrap();
    assert_eq!(
        git.action,
        Action::Adopt,
        "an existing file is adopted, not created"
    );

    apply_on(&h);
    let backup = read(&h, "/home/t/.gitconfig.bedouin-bak").expect("the original is kept");
    assert!(backup.contains("name = mine"));
    assert!(read(&h, "/home/t/.gitconfig")
        .unwrap()
        .contains("editor = nvim"));
}

#[test]
fn bedouin_refuses_to_write_through_a_symlink() {
    // Writing through one puts content somewhere the config does not name.
    let h = fresh();
    h.symlinks
        .borrow_mut()
        .insert("/home/t/.gitconfig".into(), "/etc/passwd".into());
    let o = plan_on(&h);
    let report = apply::apply(
        &o.plan,
        &o.config,
        &o.facts,
        o.state,
        &h,
        &Default::default(),
        &mut |_| {},
    )
    .unwrap();
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
    assert!(read(&h, "/home/t/.gitconfig").is_none());
    // The PATH file stays, and should: the config still declares a rust
    // toolchain, and ~/.cargo/bin is where that toolchain puts what it
    // installs. Dropping a package's `path:` does not un-declare rust.
    let path_file =
        read(&h, "/home/t/.zshrc.d/00-bedouin-path.zsh").expect("toolchain still declared");
    assert!(path_file.contains(".cargo/bin"), "{path_file}");

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
    assert!(read(&h, "/home/t/.gitconfig")
        .unwrap()
        .contains("editor = nvim"));
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
    assert!(read(&h, "/home/t/.zshrc")
        .unwrap()
        .contains("bedouin: source"));

    let h = without_shell_files(h);
    apply_on(&h);
    let rc = read(&h, "/home/t/.zshrc").unwrap();
    assert_eq!(
        rc, "export EDITOR=vi\nalias ll='ls -l'\n",
        "restored exactly"
    );
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
        !o.plan
            .items
            .iter()
            .any(|i| i.name == "jq" && i.action == Action::Remove),
        "a pre-existing package must not be planned for removal"
    );
}

// ---------------------------------------------------------------------------
// Regressions from the M1 executor review. Sixteen of its findings were
// data-loss; these are the ones that destroyed something a user cannot get
// back, or left a machine no re-run could fix.
// ---------------------------------------------------------------------------

fn with_config(h: FakeHost, cfg: &str) -> FakeHost {
    h.with_file("/cfg/bedouin.yaml", cfg)
}

#[test]
fn an_rc_block_never_truncates_the_file_it_writes_into() {
    // The worst bug in the milestone. Every package rc block was planned as
    // "bedouin owns this whole file", and the executor then upserted into an
    // EMPTY string -- so a block aimed at the user's own ~/.zshrc replaced it
    // with a single bedouin block, no backup, and `apply` reported success.
    let h = with_config(
        fresh().with_file("/home/t/.zshrc", "# my life's work\nexport EDITOR=vim\n"),
        r#"
version: 0
shell: zsh
packages:
  - name: jq
    from: apt
    rc: [{ file: "{{ home }}/.zshrc", content: "alias k=kubectl" }]
"#,
    );
    apply_on(&h);
    let rc = read(&h, "/home/t/.zshrc").unwrap();
    assert!(
        rc.contains("# my life's work"),
        "the user's file survives: {rc}"
    );
    assert!(rc.contains("export EDITOR=vim"));
    assert!(rc.contains("alias k=kubectl"));
    assert!(
        rc.contains("bedouin: source"),
        "and bedouin's own block too"
    );
}

#[test]
fn two_packages_may_share_one_drop_in_file() {
    // The documented `{{ shell.rc_dir }}/...` pattern, just shared. Both items
    // believed they owned the whole file, so the first package's block was
    // silently overwritten and `plan` reported convergence forever.
    let h = with_config(
        fresh(),
        r#"
version: 0
shell: zsh
packages:
  - name: jq
    from: apt
    rc: [{ file: "{{ shell.rc_dir }}/50-tools.zsh", content: "alias j=jq" }]
  - name: ripgrep
    from: apt
    rc: [{ file: "{{ shell.rc_dir }}/50-tools.zsh", content: "alias r=rg" }]
"#,
    )
    .with_command(
        "sudo -n apt-get install -y ripgrep",
        FakeRun::ok("Setting up ripgrep"),
    );
    let report = apply_on(&h);
    assert!(report.ok(), "{:?}", report.failure);
    let f = read(&h, "/home/t/.zshrc.d/50-tools.zsh").unwrap();
    assert!(f.contains("alias j=jq"), "first package's block: {f}");
    assert!(f.contains("alias r=rg"), "second package's block: {f}");
    assert_eq!(plan_on(&h).plan.exit_code(), 0);
}

#[test]
fn editing_managed_content_actually_takes_effect() {
    // The diff asked only whether an id was in state, so the hash the executor
    // recorded was never read back. Every managed file and rc block was
    // write-once: editing the template printed "No changes" forever.
    let h = fresh();
    apply_on(&h);
    assert!(read(&h, "/home/t/.gitconfig")
        .unwrap()
        .contains("editor = nvim"));

    let h = h.with_file(
        "/cfg/bedouin.yaml",
        &CONFIG.replace("editor: nvim", "editor: helix"),
    );
    let o = plan_on(&h);
    assert_eq!(
        o.plan.exit_code(),
        2,
        "the edit must be visible:\n{}",
        o.plan.render(true)
    );
    apply_on(&h);
    assert!(read(&h, "/home/t/.gitconfig")
        .unwrap()
        .contains("editor = helix"));
}

#[test]
fn dropping_one_path_entry_does_not_delete_the_others() {
    // Each `path/{entry}` item recorded the same shared generated file as its
    // own, so removing one entry deleted the whole file -- and the survivors
    // were all `complete` in state, so nothing ever rewrote it.
    let two = CONFIG.replace(
        "    path: [\"{{ home }}/.cargo/bin\"]",
        "    path: [\"{{ home }}/.cargo/bin\", \"{{ home }}/.local/bin\"]",
    );
    let h = with_config(fresh(), &two);
    apply_on(&h);
    let f = read(&h, "/home/t/.zshrc.d/00-bedouin-path.zsh").unwrap();
    assert!(f.contains(".cargo/bin") && f.contains(".local/bin"));

    let h = with_config(h, CONFIG);
    apply_on(&h);
    let f = read(&h, "/home/t/.zshrc.d/00-bedouin-path.zsh")
        .expect("the PATH file must survive dropping one entry");
    assert!(
        f.contains(".cargo/bin"),
        "the surviving entry is still there: {f}"
    );
    assert!(!f.contains(".local/bin"), "the dropped one is gone: {f}");
    assert_eq!(plan_on(&h).plan.exit_code(), 0);
}

#[test]
fn a_failed_step_does_not_erase_what_state_knew() {
    // The intent marker replaced the whole record instead of flipping its
    // status, discarding `method`, `backup` and `owned_files`. A package
    // bedouin installed then became permanently unowned: dropping it from the
    // config ran no uninstaller.
    let h = fresh();
    apply_on(&h);

    // Now make the *next* run fail on that same item, by changing its version.
    let bumped = CONFIG.replace(
        "  - name: jq\n    from: apt\n",
        "  - name: jq\n    from: apt\n    version: \"1.7\"\n",
    );
    let h = with_config(h, &bumped).with_command(
        "sudo -n apt-get install -y jq=1.7",
        FakeRun::fails(100, "no such version"),
    );
    let o = plan_on(&h);
    apply::apply(
        &o.plan,
        &o.config,
        &o.facts,
        o.state,
        &h,
        &Default::default(),
        &mut |_| {},
    )
    .unwrap();

    let v: serde_json::Value =
        serde_json::from_str(&read(&h, "/home/t/.local/state/bedouin/state.json").unwrap())
            .unwrap();
    let jq = &v["items"]["package/jq"];
    assert_eq!(jq["status"], "incomplete");
    assert_eq!(
        jq["method"], "apt",
        "how it was installed must survive the failure"
    );
    assert_eq!(jq["owner"], "bedouin", "and so must ownership");
}

#[test]
fn a_backup_is_never_overwritten_by_bedouins_own_render() {
    // A re-adopt copied bedouin's rendered content over the real backup,
    // destroying the only copy of the user's file.
    let h = fresh().with_file("/home/t/.gitconfig", "[user]\n\tname = mine\n");
    apply_on(&h);
    let backup = "/home/t/.gitconfig.bedouin-bak";
    assert!(read(&h, backup).unwrap().contains("name = mine"));

    // Force a second adopt by clearing state, which is what the tool's own
    // corrupt-state message tells a user to do.
    h.files.borrow_mut().remove(std::path::Path::new(
        "/home/t/.local/state/bedouin/state.json",
    ));
    apply_on(&h);
    assert!(
        read(&h, backup).unwrap().contains("name = mine"),
        "the user's original must still be the thing in the backup"
    );
}

#[test]
fn the_backup_path_appends_rather_than_replacing_the_extension() {
    // `with_extension` turned `init.lua` into `init.bedouin-bak`, so two
    // managed files in one directory sharing a stem collided on one backup.
    let h = with_config(
        fresh()
            .with_file("/cfg/templates/a.j2", "A\n")
            .with_file("/home/t/.config/x/init.lua", "the user's lua\n"),
        r#"
version: 0
shell: zsh
packages: [{ name: jq, from: apt }]
files:
  - src: templates/a.j2
    dest: ~/.config/x/init.lua
"#,
    );
    apply_on(&h);
    assert_eq!(
        read(&h, "/home/t/.config/x/init.lua.bedouin-bak").as_deref(),
        Some("the user's lua\n")
    );
}

#[test]
fn a_removal_whose_uninstaller_fails_does_not_wedge_every_future_run() {
    // Stop-on-first-failure plus "drop the record only on success" meant the
    // same doomed command ran first on every future apply, and nothing after it
    // ever executed again.
    let h = fresh();
    apply_on(&h);
    let h = with_config(
        h.with_command(
            "sudo -n apt-get remove -y jq",
            FakeRun::fails(100, "not installed"),
        ),
        &CONFIG.replace("  - name: jq\n    from: apt\n", ""),
    );
    let report = apply_on(&h);
    assert!(report.ok(), "the run must continue: {:?}", report.failure);
    assert_eq!(plan_on(&h).plan.exit_code(), 0, "and converge");
}

#[test]
fn rc_and_path_writes_refuse_a_symlink_too() {
    // The refusal existed only for `files:`, and OsHost::write renames over the
    // path -- so a ~/.zshrc symlinked into a dotfiles repo was silently severed.
    let h = fresh();
    h.symlinks
        .borrow_mut()
        .insert("/home/t/.zshrc".into(), "/repo/zshrc".into());
    let o = plan_on(&h);
    let report = apply::apply(
        &o.plan,
        &o.config,
        &o.facts,
        o.state,
        &h,
        &Default::default(),
        &mut |_| {},
    )
    .unwrap();
    let f = report.failure.as_ref().expect("must refuse");
    assert!(f.message.contains("symlink"), "{}", f.message);
}

#[test]
fn a_file_that_is_not_utf8_is_refused_rather_than_mangled() {
    // Every read went through `from_utf8_lossy` and the result was written
    // straight back, replacing each stray byte with U+FFFD.
    let h = fresh();
    h.files
        .borrow_mut()
        .insert("/home/t/.zshrc".into(), vec![0xff, 0xfe, b'\n']);
    let o = plan_on(&h);
    let report = apply::apply(
        &o.plan,
        &o.config,
        &o.facts,
        o.state,
        &h,
        &Default::default(),
        &mut |_| {},
    )
    .unwrap();
    let f = report.failure.as_ref().expect("must refuse");
    assert!(f.message.contains("UTF-8"), "{}", f.message);
    assert_eq!(
        h.files.borrow()[std::path::Path::new("/home/t/.zshrc")],
        vec![0xff, 0xfe, b'\n'],
        "and leave the bytes alone"
    );
}

#[test]
fn from_rustup_is_refused_rather_than_installing_a_toolchain() {
    // `recipe::install(Rustup, ..)` ignores the package name entirely, so
    // `from: rustup` installed the Rust toolchain in place of whatever the
    // config asked for and reported success.
    let h = with_config(
        fresh(),
        "version: 0\nshell: zsh\npackages: [{ name: ripgrep, from: rustup }]\n",
    );
    let e = run::plan_for(
        &h,
        Some(Path::new("/cfg/bedouin.yaml")),
        Path::new("/cfg"),
        Os::Linux,
        Arch::X86_64,
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("toolchains, not packages"), "{e}");
}

#[test]
fn a_toolchain_already_on_the_machine_is_on_every_later_step_path() {
    // Found by inferring a config for a real machine. PATH was assembled from
    // the state manifest alone, so a toolchain bedouin did NOT install
    // contributed nothing -- and `installer: rustup` on a box that already had
    // rustup failed with "No such file or directory", which is the confusing
    // half of the very problem the constructed environment exists to solve.
    let h = with_config(
        fresh()
            .with_binary("/home/t/.cargo/bin/rustup")
            .with_binary("/home/t/.cargo/bin/cargo"),
        r#"
version: 0
shell: zsh
languages:
  - name: rust
    installer: rustup
packages:
  - name: zellij
    from: cargo
"#,
    );
    apply_on(&h);
    let ran = h.ran.borrow();
    let cargo_step = ran
        .iter()
        .find(|c| c.display().contains("cargo install"))
        .expect("the cargo step ran");
    assert!(
        cargo_step.env["PATH"].contains("/home/t/.cargo/bin"),
        "a pre-existing toolchain must still be reachable: {}",
        cargo_step.env["PATH"]
    );

    // ...and it is adopted into state WITH its bin directories, so the next
    // run does not depend on re-probing the machine.
    let v: serde_json::Value =
        serde_json::from_str(&read(&h, "/home/t/.local/state/bedouin/state.json").unwrap())
            .unwrap();
    let rust = &v["items"]["language/rust"];
    assert_eq!(rust["bin_dirs"][0], "/home/t/.cargo/bin");
}

// ---- repos: config that lives in a git repository (§20) -------------------

#[test]
fn a_repo_is_cloned_once_and_then_left_alone() {
    // Present is done, the same rule as `version: latest`. Pulling on every
    // apply would make plan claim a change it cannot know about without going
    // to the network, and it would never converge.
    let cfg = r#"
version: 0
shell: zsh
packages: [{ name: jq, from: apt }]
repos:
  - url: https://example.invalid/nvim-config
    dest: ~/.config/nvim
    ref: main
"#;
    let h = with_config(fresh(), cfg).with_command(
        "git clone --depth 1 --branch main https://example.invalid/nvim-config /home/t/.config/nvim",
        FakeRun::ok("Cloning into '/home/t/.config/nvim'..."),
    );
    let report = apply_on(&h);
    assert!(report.ok(), "{:?}", report.failure);
    assert!(h
        .ran
        .borrow()
        .iter()
        .any(|c| c.display().contains("git clone")));

    // FakeHost has no real filesystem, so stand in for the clone's marker.
    h.files
        .borrow_mut()
        .insert("/home/t/.config/nvim/.git".into(), b"x".to_vec());
    let after = plan_on(&h);
    let repo = after
        .plan
        .items
        .iter()
        .find(|i| i.id.contains("nvim"))
        .expect("the repo item");
    assert_eq!(repo.action, Action::NoOp, "present and ours is done");
}

#[test]
fn a_directory_that_was_already_there_is_adopted_not_clobbered() {
    // Someone's hand-managed nvim config is not bedouin's to overwrite --
    // exactly the data-loss class the M1 review was about.
    let cfg = r#"
version: 0
shell: zsh
packages: [{ name: jq, from: apt }]
repos:
  - url: https://example.invalid/nvim-config
    dest: ~/.config/nvim
"#;
    let h =
        with_config(fresh(), cfg).with_file("/home/t/.config/nvim/init.lua", "-- mine, by hand\n");
    let o = plan_on(&h);
    let repo = o.plan.items.iter().find(|i| i.id.contains("nvim")).unwrap();
    assert_eq!(repo.action, Action::NoOp);
    assert!(repo.detail.contains("adopted"), "{}", repo.detail);

    apply_on(&h);
    assert_eq!(
        read(&h, "/home/t/.config/nvim/init.lua").as_deref(),
        Some("-- mine, by hand\n"),
        "untouched"
    );
    assert!(
        !h.ran
            .borrow()
            .iter()
            .any(|c| c.display().contains("git clone")),
        "and nothing was cloned over it"
    );
}

#[test]
fn a_repo_may_not_be_cloned_outside_your_home() {
    let cfg = r#"
version: 0
shell: zsh
packages: [{ name: jq, from: apt }]
repos:
  - url: https://example.invalid/x
    dest: /etc/nvim
"#;
    let h = with_config(fresh(), cfg);
    let e = run::plan_for(
        &h,
        Some(Path::new("/cfg/bedouin.yaml")),
        Path::new("/cfg"),
        Os::Linux,
        Arch::X86_64,
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("outside your home directory"), "{e}");
}

#[test]
fn changing_the_remote_replaces_the_clone_rather_than_pulling_into_it() {
    // Two remotes at one path is not a thing.
    let one = r#"
version: 0
shell: zsh
packages: [{ name: jq, from: apt }]
repos:
  - url: https://example.invalid/old
    dest: ~/.config/nvim
"#;
    let h = with_config(fresh(), one).with_command(
        "git clone --depth 1 https://example.invalid/old /home/t/.config/nvim",
        FakeRun::ok(""),
    );
    apply_on(&h);

    let h = with_config(h, &one.replace("/old", "/new")).with_command(
        "git clone --depth 1 https://example.invalid/new /home/t/.config/nvim",
        FakeRun::ok(""),
    );
    let o = plan_on(&h);
    let repo = o.plan.items.iter().find(|i| i.id.contains("nvim")).unwrap();
    assert!(
        matches!(repo.action, Action::Reinstall { .. }),
        "a changed remote is a reinstall, got {:?}",
        repo.action
    );
}

// ---- links: symlinks bedouin owns (§22) -----------------------------------

const LINKS: &str = r#"
version: 0
shell: zsh
packages: [{ name: jq, from: apt }]
links:
  - src: "{{ home }}/.tmux/.tmux.conf"
    dest: "{{ home }}/.tmux.conf"
"#;

#[test]
fn a_link_is_created_and_points_where_it_should() {
    // How oh-my-tmux installs, and how a config living in a subdirectory of a
    // repository gets into place.
    let h = with_config(fresh(), LINKS);
    apply_on(&h);
    assert_eq!(
        h.symlinks.borrow().get(Path::new("/home/t/.tmux.conf")),
        Some(&std::path::PathBuf::from("/home/t/.tmux/.tmux.conf"))
    );
    assert_eq!(plan_on(&h).plan.exit_code(), 0, "and converges");
}

#[test]
fn a_link_never_replaces_something_bedouin_did_not_make() {
    // §9.1 exists because a first apply once destroyed a ~/.gitconfig. A link
    // is no different.
    let h = with_config(fresh(), LINKS).with_file("/home/t/.tmux.conf", "# mine, by hand\n");
    let e = run::plan_for(
        &h,
        Some(Path::new("/cfg/bedouin.yaml")),
        Path::new("/cfg"),
        Os::Linux,
        Arch::X86_64,
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("not a link bedouin made"), "{e}");
    assert!(e.contains("move it aside"), "says what to do: {e}");

    // Someone else's symlink is refused too, and differently.
    let h2 = with_config(fresh(), LINKS);
    h2.symlinks
        .borrow_mut()
        .insert("/home/t/.tmux.conf".into(), "/somewhere/else".into());
    let e = run::plan_for(
        &h2,
        Some(Path::new("/cfg/bedouin.yaml")),
        Path::new("/cfg"),
        Os::Linux,
        Arch::X86_64,
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("already a symlink"), "{e}");
}

#[test]
fn removing_a_link_does_not_follow_it() {
    // The link is bedouin's; what it points at is not.
    let h = with_config(fresh(), LINKS).with_file("/home/t/.tmux/.tmux.conf", "# upstream\n");
    apply_on(&h);
    let h = with_config(
        h,
        "version: 0\nshell: zsh\npackages: [{ name: jq, from: apt }]\n",
    );
    apply_on(&h);
    assert!(h
        .symlinks
        .borrow()
        .get(Path::new("/home/t/.tmux.conf"))
        .is_none());
    assert_eq!(
        read(&h, "/home/t/.tmux/.tmux.conf").as_deref(),
        Some("# upstream\n"),
        "what it pointed at survives"
    );
}

#[test]
fn repointing_a_link_bedouin_owns_is_an_update() {
    let h = with_config(fresh(), LINKS);
    apply_on(&h);
    let h = with_config(
        h,
        &LINKS.replace(".tmux/.tmux.conf", ".tmux/.tmux.conf.local"),
    );
    let o = plan_on(&h);
    let link = o
        .plan
        .items
        .iter()
        .find(|i| i.id.contains(".tmux.conf"))
        .unwrap();
    assert!(
        matches!(link.action, Action::Upgrade { .. }),
        "{:?}",
        link.action
    );
    apply_on(&h);
    assert_eq!(
        h.symlinks.borrow().get(Path::new("/home/t/.tmux.conf")),
        Some(&std::path::PathBuf::from("/home/t/.tmux/.tmux.conf.local"))
    );
}

#[test]
fn skip_holds_a_step_back_without_stranding_the_rest() {
    // One package from a repository the machine has not set up should not
    // strand the other fifty steps.
    let h = fresh();
    let o = plan_on(&h);
    let skip: std::collections::BTreeSet<String> = ["jq".to_string()].into_iter().collect();
    let report = apply::apply(
        &o.plan,
        &o.config,
        &o.facts,
        o.state,
        &h,
        &skip,
        &mut |_: Line| {},
    )
    .expect("apply");

    assert_eq!(report.skipped, ["package/jq"], "{:?}", report.skipped);
    assert!(report.ok(), "the rest of the run should still succeed");
    assert!(
        report.completed.iter().any(|id| id == "language/rust"),
        "other steps must still run: {:?}",
        report.completed
    );
    assert!(
        !report.completed.iter().any(|id| id == "package/jq"),
        "the skipped step must not be reported as completed"
    );
    // And it is named, not silently dropped.
    assert!(report.render().contains("package/jq"));
}

#[test]
fn skip_takes_the_full_id_too() {
    let h = fresh();
    let o = plan_on(&h);
    let skip: std::collections::BTreeSet<String> = ["package/jq".to_string()].into_iter().collect();
    let report = apply::apply(
        &o.plan,
        &o.config,
        &o.facts,
        o.state,
        &h,
        &skip,
        &mut |_: Line| {},
    )
    .expect("apply");
    assert_eq!(report.skipped, ["package/jq"]);
}

#[test]
fn a_language_brings_its_own_installer_without_being_declared() {
    // The failure this prevents: `installer: rustup` with no
    // `package_managers:` entry planned the toolchain install and never
    // planned rustup, so a fresh machine ran `rustup toolchain install`
    // against a binary nothing had installed.
    const CFG: &str = r#"
version: 0
shell: zsh
languages:
  - name: rust
packages:
  - name: eza
    from: cargo
"#;
    let h = FakeHost::new()
        .with_file("/cfg/bedouin.yaml", CFG)
        .with_env("PATH", "/usr/bin:/bin")
        .with_env("HOME", "/home/t")
        .with_command("id -u", FakeRun::ok("1000"));
    let o = plan_on(&h);

    let ids: Vec<&str> = o.plan.changes().map(|i| i.id.as_str()).collect();
    assert!(
        ids.contains(&"manager/rustup"),
        "rustup must be planned, not assumed: {ids:?}"
    );
    // ...and before the toolchain that needs it.
    let m = ids.iter().position(|i| *i == "manager/rustup").unwrap();
    let l = ids.iter().position(|i| *i == "language/rust").unwrap();
    assert!(m < l, "rustup must be installed before it is used: {ids:?}");

    // A language installs with its own tool by default. rustup is how Rust is
    // meant to arrive, and what `rustup component add` later expects.
    let rust = o.config.languages.iter().find(|l| l.name == "rust");
    assert!(rust.is_some(), "rust language resolved");
    let detail = o
        .plan
        .changes()
        .find(|i| i.id == "language/rust")
        .map(|i| i.detail.clone())
        .unwrap();
    assert!(detail.contains("rustup"), "default installer: {detail}");
}

#[test]
fn a_script_package_installs_and_is_never_claimed_as_owned() {
    // For a thing no manager packages: tailscale's installer registers a
    // repository and starts a daemon, and neither is a `brew install`.
    const CFG: &str = r#"
version: 0
shell: zsh
packages:
  - name: widget
    script: |
      echo installing widget
"#;
    let h = FakeHost::new()
        .with_file("/cfg/bedouin.yaml", CFG)
        .with_env("HOME", "/home/t")
        .with_env("PATH", "/usr/bin:/bin")
        .with_command("id -u", FakeRun::ok("1000"))
        .with_command("sh /tmp/bedouin-install-widget.sh", FakeRun::ok("done"));

    let o = plan_on(&h);
    let item = o
        .plan
        .changes()
        .find(|i| i.id == "package/widget")
        .expect("planned");
    assert_eq!(item.detail, "script");

    let report = apply::apply(
        &o.plan,
        &o.config,
        &o.facts,
        o.state,
        &h,
        &Default::default(),
        &mut |_: Line| {},
    )
    .expect("apply");
    assert!(report.ok(), "{:?}", report.failure);

    // Bedouin cannot undo a script, so it must not record itself as the owner
    // -- that is what `remove` reads to decide it may uninstall something.
    let st = bedouin_core::state::load(&h, Path::new("/home/t/.local/state/bedouin/state.json"))
        .expect("state");
    let rec = st.items.get("package/widget").expect("recorded");
    assert!(rec.method.is_none(), "no method means nothing to undo with");
}

#[test]
fn from_and_script_together_is_refused() {
    const CFG: &str = r#"
version: 0
shell: zsh
packages:
  - name: widget
    from: apt
    script: "echo hi"
"#;
    let h = FakeHost::new()
        .with_file("/cfg/bedouin.yaml", CFG)
        .with_env("HOME", "/home/t")
        .with_command("id -u", FakeRun::ok("1000"));
    let e = run::plan_for(
        &h,
        Some(Path::new("/cfg/bedouin.yaml")),
        Path::new("/cfg"),
        Os::Linux,
        Arch::X86_64,
    )
    .expect_err("ambiguous");
    assert!(e.to_string().contains("two ways"), "{e}");
}
