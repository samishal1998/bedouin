//! The M0 acceptance test: one config in git, planned on two fresh machines.
//!
//! This is the claim the whole product rests on, so it is checked end to end
//! through the real loader, resolver, and planner rather than by unit-testing
//! the pieces that make it work.

use bedouin_core::facts::{Arch, Os, Privilege, Shell};
use bedouin_core::host::{FakeHost, FakeRun};
use bedouin_core::plan::Action;
use bedouin_core::run;
use std::path::Path;

/// Exercises arms, declared targets, `only:`, `needs:`, rc blocks and PATH.
const CONFIG: &str = r#"
version: 0
shell: zsh

vars:
  editor: nvim

targets:
  - name: noble
    match: { distro: ubuntu, distro_version: ">=24.04" }
    vars: { editor: nvim }

package_managers:
  macos: [brew]
  default: [apt]

languages:
  - name: rust
    version: "1.80"
    installer: rustup

packages:
  - name: build-essential
    from: apt
    only: linux

  - name: zellij
    from: cargo
    version: latest
    needs: [build-essential]
    path: ["{{ home }}/.cargo/bin"]
    rc:
      - file: "{{ shell.rc_dir }}/70-zellij.zsh"
        content: |
          eval "$(zellij setup --generate-auto-start zsh)"

  - name: fd
    from:
      macos: brew
      default: apt

  - name: neovim
    from: { noble: apt, default: cargo }

  - name: xclip
    from: apt
    only: linux

  - name: mas
    from: brew
    only: macos

files:
  - src: templates/gitconfig.j2
    dest: ~/.gitconfig
"#;

fn machine(os: Os) -> FakeHost {
    let mut h = FakeHost::new()
        .with_file("/cfg/bedouin.yaml", CONFIG)
        // `src:` resolves against the config root, and plan refuses to name a
        // source that is not there -- a plan apply cannot keep is not a plan.
        .with_file("/cfg/templates/gitconfig.j2", "[core]\n\teditor = {{ vars.editor }}\n")
        .with_env("HOME", "/home/t")
        .with_env("USER", "t")
        .with_env("HOSTNAME", "khaymah")
        .with_env("SHELL", "/bin/bash") // a fresh box, still on bash
        .with_env("PATH", "/usr/bin:/bin")
        .with_command("id -u", FakeRun::ok("1000"))
        .with_command("sudo -n true", FakeRun::ok(""));
    if os == Os::Linux {
        h = h
            .with_file(
                "/etc/os-release",
                "ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n",
            )
            .with_binary("/usr/bin/apt-get");
    }
    h
}

fn plan_on(os: Os, arch: Arch) -> run::Outcome {
    let h = machine(os);
    run::plan_for(&h, Some(Path::new("/cfg/bedouin.yaml")), Path::new("/cfg"), os, arch)
        .unwrap_or_else(|e| panic!("planning on {os} failed: {e}"))
}

fn named<'a>(o: &'a run::Outcome, name: &str) -> Option<&'a bedouin_core::plan::Item> {
    o.plan.items.iter().find(|i| i.name == name)
}

#[test]
fn one_config_covers_ubuntu_and_macos() {
    let linux = plan_on(Os::Linux, Arch::X86_64);
    let mac = plan_on(Os::Macos, Arch::Arm64);

    // `only:` decides membership, which arms cannot express. Without it this
    // single config could not cover both machines at all.
    assert!(named(&linux, "xclip").is_some(), "xclip is a Linux package");
    assert!(named(&linux, "mas").is_none(), "mas must not appear on Linux");
    assert!(linux.config.pruned.iter().any(|p| p == "package/mas"));

    assert!(named(&mac, "mas").is_some(), "mas is a macOS package");
    assert!(named(&mac, "xclip").is_none(), "xclip must not appear on macOS");
    assert!(mac.config.pruned.iter().any(|p| p == "package/xclip"));

    // A pruned item's other fields are never resolved, so `from: apt` on a
    // Linux-only package costs nothing on a machine with no apt.
    assert!(mac.config.pruned.iter().any(|p| p == "package/build-essential"));
}

#[test]
fn arms_and_targets_pick_the_right_source_per_machine() {
    let linux = plan_on(Os::Linux, Arch::X86_64);
    let mac = plan_on(Os::Macos, Arch::Arm64);

    assert!(named(&linux, "fd").unwrap().detail.contains("apt"));
    assert!(named(&mac, "fd").unwrap().detail.contains("brew"));

    // The `noble` target beats the `default` arm on Ubuntu 24.04: a declared
    // target always wins, because you named it deliberately.
    assert!(named(&linux, "neovim").unwrap().detail.contains("apt"));
    assert!(named(&mac, "neovim").unwrap().detail.contains("cargo"));

    // A manager that cannot exist on this OS is dropped rather than planned.
    assert!(named(&mac, "apt").is_none(), "apt must never be planned on macOS");
    assert!(named(&linux, "brew").is_none());
}

#[test]
fn a_declared_shell_beats_the_shell_the_user_is_running() {
    // The fresh-box case: invoked from bash, configuring zsh. Keying rc paths
    // off the detected shell would write into the shell being replaced.
    let linux = plan_on(Os::Linux, Arch::X86_64);
    assert_eq!(linux.facts.shell.name, Shell::Zsh);
    assert_eq!(linux.facts.shell.detected, Shell::Bash);
    assert!(named(&linux, "~/.zshrc.d/70-zellij.zsh").is_some());
}

#[test]
fn a_needs_edge_to_a_pruned_package_simply_does_not_apply() {
    // `zellij needs build-essential` is right on Linux and meaningless on
    // macOS, and one config has to say both. A prerequisite pruned by `only:`
    // drops the edge; one that was never declared is still an error.
    let mac = plan_on(Os::Macos, Arch::Arm64);
    assert!(named(&mac, "zellij").is_some());
    assert!(named(&mac, "build-essential").is_none());

    let bad = CONFIG.replace("needs: [build-essential]", "needs: [nonexistent]");
    let h = machine(Os::Linux).with_file("/cfg/bedouin.yaml", &bad);
    let err = run::plan_for(
        &h,
        Some(Path::new("/cfg/bedouin.yaml")),
        Path::new("/cfg"),
        Os::Linux,
        Arch::X86_64,
    )
    .unwrap_err();
    assert!(err.message.contains("nonexistent"), "{err}");
}

#[test]
fn build_prerequisites_are_ordered_before_what_needs_them() {
    // Nothing in `from: cargo` says zellij wants a C toolchain, so the edge is
    // declared. Without it both sit in one unordered stage.
    let linux = plan_on(Os::Linux, Arch::X86_64);
    let pos = |n: &str| linux.plan.items.iter().position(|i| i.name == n).unwrap();
    assert!(
        pos("build-essential") < pos("zellij"),
        "the prerequisite must be planned first"
    );
}

#[test]
fn a_fresh_machine_plans_everything_and_exits_two() {
    let linux = plan_on(Os::Linux, Arch::X86_64);
    assert!(linux.state.items.is_empty(), "no state file means a first run");
    // Every package is new. `apt` is a no-op because a fresh Ubuntu already
    // has it -- and it is never bootstrapped, only used.
    assert!(
        linux
            .plan
            .items
            .iter()
            .filter(|i| i.kind == bedouin_core::state::ItemKind::Package)
            .all(|i| i.action == Action::Create),
        "nothing is installed yet, so every package is a create"
    );
    assert_eq!(named(&linux, "apt").map(|i| i.action.clone()), Some(Action::NoOp));
    // Exit 2 is what makes `plan` usable as a CI drift check.
    assert_eq!(linux.plan.exit_code(), 2);

    let out = linux.plan.render(false);
    assert!(out.contains("Bedouin will make the following changes:"), "{out}");
    assert!(out.contains("+ package"), "{out}");
    assert!(out.contains("to add"), "{out}");
    assert_eq!(linux.facts.privilege, Privilege::Passwordless);
}

#[test]
fn a_managed_file_whose_template_is_missing_is_refused() {
    // `plan` claims to be a faithful prediction of `apply`. Naming a source
    // that does not exist is a promise apply cannot keep, and checking is free.
    let cfg = CONFIG.replace("templates/gitconfig.j2", "templates/absent.j2");
    let h = machine(Os::Linux).with_file("/cfg/bedouin.yaml", &cfg);
    let err = run::plan_for(
        &h,
        Some(Path::new("/cfg/bedouin.yaml")),
        Path::new("/cfg"),
        Os::Linux,
        Arch::X86_64,
    )
    .unwrap_err();
    assert!(err.message.contains("does not exist"), "{err}");
    assert!(err.message.contains("/cfg/templates/absent.j2"), "{err}");
}

#[test]
fn a_template_outside_the_config_root_is_refused() {
    let cfg = CONFIG.replace("templates/gitconfig.j2", "../../etc/passwd");
    let h = machine(Os::Linux).with_file("/cfg/bedouin.yaml", &cfg);
    let err = run::plan_for(
        &h,
        Some(Path::new("/cfg/bedouin.yaml")),
        Path::new("/cfg"),
        Os::Linux,
        Arch::X86_64,
    )
    .unwrap_err();
    assert!(err.message.contains("outside the config root"), "{err}");
}

#[test]
fn paths_and_rc_files_render_against_resolved_facts() {
    let linux = plan_on(Os::Linux, Arch::X86_64);
    assert!(
        named(&linux, "~/.cargo/bin").is_some(),
        "the PATH entry rendered from {{{{ home }}}}"
    );
    assert!(named(&linux, "~/.gitconfig").is_some(), "the managed file");
    assert_eq!(linux.config.vars.get("editor").map(String::as_str), Some("nvim"));
}

#[test]
fn verbose_output_names_the_arm_that_won() {
    // The only visible trace of arm selection, and what a user reaches for when
    // a config resolves differently than expected.
    let linux = plan_on(Os::Linux, Arch::X86_64);
    let out = linux.plan.render(true);
    assert!(out.contains("from = noble"), "{out}");
    assert!(out.contains("Not on this machine:"), "{out}");
    assert!(out.contains("package/mas"), "{out}");
}

#[test]
fn an_implied_toolchain_is_added_with_a_warning_not_silently() {
    let cfg = CONFIG.replace(
        "languages:\n  - name: rust\n    version: \"1.80\"\n    installer: rustup\n",
        "",
    );
    let h = machine(Os::Linux).with_file("/cfg/bedouin.yaml", &cfg);
    let o = run::plan_for(
        &h,
        Some(Path::new("/cfg/bedouin.yaml")),
        Path::new("/cfg"),
        Os::Linux,
        Arch::X86_64,
    )
    .unwrap();
    assert!(named(&o, "rust").is_some(), "`from: cargo` implies rust");
    assert!(
        o.plan.warnings.iter().any(|w| w.contains("implicitly")),
        "the implication must be stated, not silent: {:?}",
        o.plan.warnings
    );
}
