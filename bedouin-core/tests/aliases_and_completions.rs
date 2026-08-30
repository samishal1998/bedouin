//! Aliases and completions (spec §16).
//!
//! Both are things a dotfiles manager is expected to do and both are awkward
//! as raw `rc:` content, because the syntax is shell-specific and the quoting
//! is easy to get wrong in a way that only breaks at login.

use bedouin_core::apply;
use bedouin_core::facts::{Arch, Os, Shell};
use bedouin_core::host::{FakeHost, FakeRun, Host, Line};
use bedouin_core::plan::Action;
use bedouin_core::run;
use bedouin_core::writers;
use std::collections::BTreeMap;
use std::path::Path;

fn machine(cfg: &str) -> FakeHost {
    FakeHost::new()
        .with_file("/cfg/bedouin.yaml", cfg)
        .with_file(
            "/etc/os-release",
            "ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n",
        )
        .with_env("HOME", "/home/t")
        .with_env("USER", "t")
        .with_env("SHELL", "/bin/zsh")
        .with_env("PATH", "/usr/bin:/bin")
        .with_binary("/usr/bin/apt-get")
        .with_binary("/usr/bin/jq")
        .with_command("id -u", FakeRun::ok("1000"))
        .with_command("sudo -n true", FakeRun::ok(""))
}

fn outcome(h: &FakeHost) -> run::Outcome {
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
    let o = outcome(h);
    apply::apply(&o.plan, &o.config, &o.facts, o.state, h, &mut |_: Line| {}).unwrap()
}

/// The three arguments every `plan_for` call in this file shares.
fn h_path() -> Option<&'static Path> {
    Some(Path::new("/cfg/bedouin.yaml"))
}

fn read(h: &FakeHost, p: &str) -> Option<String> {
    h.read(Path::new(p))
        .unwrap()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

const CFG: &str = r#"
version: 0
shell: zsh
aliases:
  ll: ls -alh
packages:
  - name: jq
    from: apt
    aliases:
      j: jq
    completions:
      generate: ["jq", "--completion", "{{ shell.name }}"]
"#;

fn fresh() -> FakeHost {
    machine(CFG).with_command(
        "jq --completion zsh",
        FakeRun::ok("#compdef jq\n_jq() { :; }"),
    )
}

#[test]
fn aliases_land_in_their_own_scope() {
    let h = fresh();
    apply_on(&h);
    // Global aliases are bedouin's own block.
    let global = read(&h, "/home/t/.zshrc.d/10-bedouin-aliases.zsh").expect("global block");
    assert!(global.contains("alias ll='ls -alh'"));
    assert!(
        !global.contains("alias j="),
        "a package's aliases are not global"
    );

    // A package's aliases live with the package.
    let pkg = read(&h, "/home/t/.zshrc.d/30-jq-aliases.zsh").expect("package block");
    assert!(pkg.contains("alias j='jq'"));
}

#[test]
fn dropping_a_package_takes_its_aliases_with_it() {
    // This is why per-package aliases are not merged into one shared file:
    // removal works through machinery that already exists and converges. A
    // shared file would reintroduce the coupling the M1 review removed.
    let h = fresh();
    apply_on(&h);
    assert!(read(&h, "/home/t/.zshrc.d/30-jq-aliases.zsh").is_some());

    // Same machine, same state, jq gone from the config.
    let h = h.with_file(
        "/cfg/bedouin.yaml",
        "version: 0\nshell: zsh\naliases:\n  ll: ls -alh\npackages: [{name: jq, from: apt}]\n",
    );
    let report = apply_on(&h);
    assert!(report.ok(), "{:?}", report.failure);
    assert!(
        read(&h, "/home/t/.zshrc.d/30-jq-aliases.zsh").is_none(),
        "the package's alias file goes with its aliases"
    );
    assert!(
        read(&h, "/home/t/.zshrc.d/completions/_jq").is_none(),
        "and so do its completions"
    );
    // The global block is untouched by any of that.
    assert!(read(&h, "/home/t/.zshrc.d/10-bedouin-aliases.zsh")
        .unwrap()
        .contains("alias ll="));
    assert_eq!(outcome(&h).plan.exit_code(), 0);
}

#[test]
fn alias_values_survive_a_shell_reading_them() {
    // Values are user text landing in a file the shell evaluates, so quoting is
    // load-bearing. An embedded quote is the case that breaks naive escaping.
    let aliases = BTreeMap::from([
        ("q".to_string(), "echo 'it's fine'".to_string()),
        ("p".to_string(), "grep --color=auto".to_string()),
    ]);
    let zsh = writers::alias_lines(&aliases, Shell::Zsh);
    assert!(zsh.contains("alias p='grep --color=auto'"));
    // The posix trick: close, escaped quote, reopen.
    assert!(zsh.contains(r"'\''"), "embedded quote escaped: {zsh}");

    // fish's alias is a function definition, and its escaping differs.
    let fish = writers::alias_lines(&aliases, Shell::Fish);
    assert!(fish.contains("alias p 'grep --color=auto'"), "{fish}");
    // fish takes words, not an `=` pair -- the `=` in the value is incidental.
    assert!(!fish.contains("alias p="), "{fish}");
    assert!(zsh.contains("alias p="), "posix shells do use one: {zsh}");
}

#[test]
fn completions_are_generated_by_the_tool_and_written_not_evaluated() {
    let h = fresh();
    apply_on(&h);
    let comp = read(&h, "/home/t/.zshrc.d/completions/_jq").expect("completions written");
    assert!(comp.contains("#compdef jq"));

    // Generated through the same argv path as every other step -- no shell.
    let ran = h.ran.borrow();
    let step = ran
        .iter()
        .find(|c| c.display().contains("--completion"))
        .unwrap();
    assert_eq!(step.argv, ["jq", "--completion", "zsh"]);
    assert!(!step.root, "generating completions needs no privilege");
}

#[test]
fn zsh_gets_its_completions_directory_onto_fpath() {
    // zsh finds completions through fpath, and fpath must be set before
    // compinit -- so it belongs in the rc file, not a drop-in sourced later.
    let h = fresh();
    apply_on(&h);
    let rc = read(&h, "/home/t/.zshrc").unwrap();
    assert!(
        rc.contains("fpath=(\"/home/t/.zshrc.d/completions\" $fpath)"),
        "{rc}"
    );
}

#[test]
fn a_generator_that_fails_or_says_nothing_is_an_error() {
    let h = machine(CFG).with_command("jq --completion zsh", FakeRun::fails(2, "unknown flag"));
    let report = apply_on(&h);
    let f = report.failure.as_ref().expect("must fail");
    assert!(f.message.contains("exited 2"), "{}", f.message);

    // Silence is also a failure: writing an empty completions file would look
    // like success and break completion for that tool.
    let h = machine(CFG).with_command("jq --completion zsh", FakeRun::ok(""));
    let report = apply_on(&h);
    assert!(report
        .failure
        .as_ref()
        .unwrap()
        .message
        .contains("no completions"));
}

#[test]
fn editing_the_generator_command_re_runs_it() {
    // Addressed on the command, since the output cannot be known before it
    // runs. Without this the item would be write-once, which is the bug the
    // M1 review found one feature over.
    let h = fresh();
    apply_on(&h);
    assert_eq!(outcome(&h).plan.exit_code(), 0);

    let h2 = h
        .with_file(
            "/cfg/bedouin.yaml",
            &CFG.replace("\"--completion\"", "\"--completion-v2\""),
        )
        .with_command(
            "jq --completion-v2 zsh",
            FakeRun::ok("#compdef jq\n_v2() { :; }"),
        );
    let o = outcome(&h2);
    let comp = o
        .plan
        .items
        .iter()
        .find(|i| i.id == "completion/jq")
        .unwrap();
    assert!(
        comp.action.is_change(),
        "the edit must be visible: {:?}",
        comp.action
    );
    apply_on(&h2);
    assert!(read(&h2, "/home/t/.zshrc.d/completions/_jq")
        .unwrap()
        .contains("_v2"));
}

#[test]
fn a_hand_edited_alias_block_reads_as_drift_and_is_repaired() {
    let h = fresh();
    apply_on(&h);
    let o = outcome(&h);
    assert!(
        bedouin_core::doctor::check(&o.state, &o.config, &o.facts, &h)
            .unwrap()
            .is_clean()
    );

    let f = "/home/t/.zshrc.d/10-bedouin-aliases.zsh";
    let edited = read(&h, f).unwrap().replace("ls -alh", "ls -la --color");
    h.files.borrow_mut().insert(f.into(), edited.into_bytes());

    let o = outcome(&h);
    let report = bedouin_core::doctor::check(&o.state, &o.config, &o.facts, &h).unwrap();
    assert!(!report.is_clean(), "a hand edit must show as drift");
    assert_eq!(report.exit_code(), 2);

    // ...and plan agrees, which is the promise doctor's message makes.
    let g = o
        .plan
        .items
        .iter()
        .find(|i| i.id == "rc/bedouin/aliases")
        .unwrap();
    assert_eq!(
        g.action,
        Action::Upgrade {
            from: "edited on disk".into(),
            to: "managed".into()
        }
    );
    apply_on(&h);
    assert!(read(&h, f).unwrap().contains("ls -alh"));
}

// ---- shell frameworks (§19) ----------------------------------------------

const FW: &str = r#"
version: 0
shell:
  name: zsh
  framework: oh-my-zsh
  theme: agnoster
  plugins: [git, docker, zsh-autosuggestions]
packages:
  - name: zsh
    from: apt
"#;

fn fw_machine() -> FakeHost {
    machine(FW)
        .with_command("sudo -n apt-get update", FakeRun::ok(""))
        .with_command("sudo -n apt-get install -y zsh", FakeRun::ok(""))
        .with_command(
            "curl -fsSL https://raw.githubusercontent.com/ohmyzsh/ohmyzsh/master/tools/install.sh -o /tmp/bedouin-omz.sh",
            FakeRun::ok(""),
        )
        .with_command("sh /tmp/bedouin-omz.sh --unattended --keep-zshrc", FakeRun::ok("done"))
}

#[test]
fn the_framework_block_goes_above_the_line_that_reads_it() {
    // oh-my-zsh reads ZSH_THEME and plugins as it loads, so a block appended
    // at the end of .zshrc -- where every other bedouin block goes -- is a
    // silent no-op on exactly the machines this feature is for.
    let h = fw_machine().with_file(
        "/home/t/.zshrc",
        "export ZSH=\"$HOME/.oh-my-zsh\"\nZSH_THEME=\"robbyrussell\"\nsource $ZSH/oh-my-zsh.sh\n# mine\n",
    );
    apply_on(&h);
    let rc = read(&h, "/home/t/.zshrc").unwrap();
    let block = rc.find(">>> bedouin: framework").expect("the block");
    let loader = rc.find("source $ZSH/oh-my-zsh.sh").unwrap();
    assert!(block < loader, "block must precede the loader:\n{rc}");
    assert!(rc.contains("ZSH_THEME='agnoster'"));
    assert!(rc.contains("plugins=(git docker zsh-autosuggestions)"));
    assert!(rc.contains("# mine"), "the user's own file survives");
}

#[test]
fn the_framework_is_installed_only_when_absent_and_never_owned() {
    let h = fw_machine();
    apply_on(&h);
    assert!(h
        .ran
        .borrow()
        .iter()
        .any(|c| c.display().contains("bedouin-omz.sh")));
    // --keep-zshrc matters: bedouin owns a BLOCK in .zshrc, and letting the
    // installer replace the file would take the user's config with it.
    assert!(h
        .ran
        .borrow()
        .iter()
        .any(|c| c.display().contains("--keep-zshrc")));

    // Present already: adopted, not reinstalled, and never removed.
    let h2 = fw_machine().with_file("/home/t/.oh-my-zsh/oh-my-zsh.sh", "# omz\n");
    let o = outcome(&h2);
    let fw = o.plan.items.iter().find(|i| i.name == "oh-my-zsh").unwrap();
    assert_eq!(fw.action, Action::NoOp);
}

#[test]
fn a_framework_on_the_wrong_shell_is_refused() {
    let h = machine("version: 0\nshell:\n  name: bash\n  framework: oh-my-zsh\npackages: [{name: jq, from: apt}]\n");
    let e = run::plan_for(&h, h_path(), Path::new("/cfg"), Os::Linux, Arch::X86_64)
        .unwrap_err()
        .to_string();
    assert!(e.contains("needs `shell: zsh`"), "{e}");

    let h = machine("version: 0\nshell:\n  name: zsh\n  framework: oh-my-fish\npackages: [{name: jq, from: apt}]\n");
    let e = run::plan_for(&h, h_path(), Path::new("/cfg"), Os::Linux, Arch::X86_64)
        .unwrap_err()
        .to_string();
    assert!(e.contains("supported: oh-my-zsh"), "{e}");
}

#[test]
fn a_theme_without_a_framework_is_refused_rather_than_ignored() {
    let h = machine(
        "version: 0\nshell:\n  name: zsh\n  theme: agnoster\npackages: [{name: jq, from: apt}]\n",
    );
    let e = run::plan_for(&h, h_path(), Path::new("/cfg"), Os::Linux, Arch::X86_64)
        .unwrap_err()
        .to_string();
    assert!(e.contains("need a `framework:`"), "{e}");
}

#[test]
fn a_plain_shell_name_still_means_what_it_did() {
    let h = machine("version: 0\nshell: zsh\npackages: [{name: jq, from: apt}]\n");
    let o = outcome(&h);
    assert_eq!(o.config.shell, Shell::Zsh);
    assert!(o.config.framework.is_none());
}
