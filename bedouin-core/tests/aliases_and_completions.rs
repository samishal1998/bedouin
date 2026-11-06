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
use std::path::{Path, PathBuf};

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
        // Every plan now installs bedouin's own completion, so a fake machine
        // has to be able to answer for it.
        .with_env("BEDOUIN_EXE", "bedouin")
        .with_command(
            "bedouin completion-script zsh",
            FakeRun::ok("#compdef bedouin"),
        )
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
    apply::apply(
        &o.plan,
        &o.config,
        &o.facts,
        o.state,
        h,
        &Default::default(),
        &mut |_: Line| {},
    )
    .unwrap()
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

#[test]
fn bedouin_installs_its_own_completion_without_being_asked() {
    // "auto add its completion in the setup process": nobody should have to
    // declare completions for the tool that is configuring their shell.
    let h = machine("version: 0\nshell: zsh\npackages: [{name: jq, from: apt}]\n");
    let o = outcome(&h);

    let item = o
        .plan
        .changes()
        .find(|i| i.id == "completion/bedouin")
        .expect("every plan carries bedouin's own completion");
    assert!(
        item.detail.contains("completion-script"),
        "generated by re-invoking bedouin: {}",
        item.detail
    );

    // And the block that makes zsh read it, which is the half that was
    // missing: a completion in a directory nothing puts on fpath is inert.
    assert!(
        o.plan.changes().any(|i| i.id == "rc/bedouin/source"),
        "the drop-in loader must exist even when nothing else writes there"
    );
    let snippet = writers::source_dir_snippet("/home/t/.zshrc.d", Shell::Zsh);
    assert!(snippet.contains("fpath="), "and it must set fpath");
}

/// Completions nobody declared (`completions:` absent): found by asking.
mod detected {
    use super::*;
    use bedouin_core::complete;

    const GH: &str = "version: 0\nshell: zsh\npackages: [{name: gh, from: apt}]\n";
    const LOCK: &str = "/home/t/.local/state/bedouin/completions.lock";

    /// gh as it really answers: help names `completion:`, and the bare
    /// positional form exits 0 with a BASH script -- gh wants `--shell`.
    fn gh(cfg: &str) -> FakeHost {
        machine(cfg)
            .with_binary("/usr/bin/gh")
            .with_command("gh --version", FakeRun::ok("gh version 2.62.0 (2024-11-14)"))
            .with_command(
                "gh --help",
                FakeRun::ok("Work seamlessly with GitHub.\n\nADDITIONAL COMMANDS\n  completion:  Generate shell completion scripts\n"),
            )
            .with_command(
                "gh completion zsh",
                FakeRun::ok("# bash completion V2 for gh\ncomplete -o default -F __start_gh gh"),
            )
            .with_command(
                "gh completion --shell zsh",
                FakeRun::ok("#compdef gh\ncompdef _gh gh\n_gh() { :; }"),
            )
    }

    fn asked(h: &FakeHost, what: &str) -> usize {
        h.ran
            .borrow()
            .iter()
            .filter(|c| c.display() == what)
            .count()
    }

    fn lock(h: &FakeHost) -> complete::Lock {
        serde_json::from_str(&read(h, LOCK).expect("a lock")).unwrap()
    }

    #[test]
    fn a_generator_the_config_does_not_name_is_found_and_set_up() {
        let h = gh(GH);
        let report = apply_on(&h);
        assert!(report.ok(), "{:?}", report.failure);

        let comp = read(&h, "/home/t/.zshrc.d/completions/_gh").expect("completions written");
        assert!(
            comp.starts_with("#compdef gh"),
            "the zsh script, not the bash one: {comp}"
        );
        assert_eq!(report.detected, ["gh"]);
        assert!(report.completed.contains(&"completion/gh".to_string()));
        assert!(report.render().contains("bedouin completions --adopt"));

        let e = &lock(&h)["gh"];
        assert_eq!(e.shell, "zsh");
        assert_eq!(e.version.as_deref(), Some("gh version 2.62.0 (2024-11-14)"));
        assert_eq!(
            e.generate.as_deref(),
            Some(&["gh", "completion", "--shell", "zsh"].map(String::from)[..])
        );

        // Questions are asked with no terminal attached: a tool that ignores
        // --help and opens a full-screen UI must not take the keyboard.
        let ran = h.ran.borrow();
        let help = ran.iter().find(|c| c.display() == "gh --help").unwrap();
        assert!(help.stdin_null, "probes must not inherit stdin");
        assert!(help.timeout.is_some_and(|t| t.as_secs() <= 10));
    }

    #[test]
    fn the_answer_is_kept_and_the_question_is_not_asked_again() {
        let h = gh(GH);
        apply_on(&h);
        let o = outcome(&h);
        let item = o
            .plan
            .items
            .iter()
            .find(|i| i.id == "completion/gh")
            .expect("planned from the lock");
        assert_eq!(item.action, Action::NoOp, "{item:?}");
        assert!(item.detail.contains("detected"), "{}", item.detail);

        let report = apply_on(&h);
        assert!(report.ok());
        assert!(report.detected.is_empty(), "found once, announced once");
        assert_eq!(
            asked(&h, "gh --help"),
            1,
            "the lock answered the second time"
        );
    }

    #[test]
    fn a_replaced_binary_is_asked_again() {
        let h = gh(GH);
        apply_on(&h);
        let generated = asked(&h, "gh completion --shell zsh");
        // Upgraded outside bedouin: same path, different file.
        let h = h.with_fingerprint("/usr/bin/gh", "/usr/bin/gh 999 2");
        let report = apply_on(&h);
        assert!(report.ok());
        assert_eq!(asked(&h, "gh --help"), 2);
        assert!(
            asked(&h, "gh completion --shell zsh") > generated,
            "regenerated: output can change by version"
        );
        assert!(
            report.detected.is_empty(),
            "not new, so not announced again"
        );
        assert_eq!(lock(&h)["gh"].binary, "/usr/bin/gh 999 2");
    }

    #[test]
    fn a_tool_without_one_is_asked_once_and_left_alone() {
        let h = machine("version: 0\nshell: zsh\npackages: [{name: tree, from: apt}]\n")
            .with_binary("/usr/bin/tree")
            .with_command(
                "tree --help",
                FakeRun::ok("usage: tree [-acdfghilnpqrstuvxACDFJQNSUX] [-L level]"),
            );
        let report = apply_on(&h);
        assert!(report.ok());
        assert!(report.detected.is_empty());
        assert_eq!(lock(&h)["tree"].generate, None, "a no is kept too");
        apply_on(&h);
        assert_eq!(asked(&h, "tree --help"), 1);
        assert!(!outcome(&h)
            .plan
            .items
            .iter()
            .any(|i| i.id == "completion/tree"));
    }

    #[test]
    fn output_that_is_not_a_script_for_this_shell_is_not_used() {
        // Advertised, but every form prints help text or a bash script.
        let h = machine("version: 0\nshell: zsh\npackages: [{name: npm, from: apt}]\n")
            .with_binary("/usr/bin/npm")
            .with_command("npm --help", FakeRun::ok("All commands:\n\n    access, adduser, audit,\n    completion, config, dedupe,\n"))
            .with_command(
                "npm completion zsh",
                FakeRun::ok("###-begin-npm-completion-###\ncomplete -o default -F _npm_completion npm\n"),
            );
        let report = apply_on(&h);
        assert!(report.ok());
        assert!(read(&h, "/home/t/.zshrc.d/completions/_npm").is_none());
        assert_eq!(lock(&h)["npm"].generate, None);
        assert_eq!(asked(&h, "npm completion --zsh"), 1, "every form was tried");
    }

    #[test]
    fn completions_false_means_never_ask() {
        let h =
            gh("version: 0\nshell: zsh\npackages: [{name: gh, from: apt, completions: false}]\n");
        let report = apply_on(&h);
        assert!(report.ok());
        assert_eq!(asked(&h, "gh --help"), 0);
        assert!(read(&h, "/home/t/.zshrc.d/completions/_gh").is_none());
    }

    #[test]
    fn a_declared_generator_is_the_answer_and_nothing_is_asked() {
        let h = gh("version: 0\nshell: zsh\npackages:\n  - name: gh\n    from: apt\n    completions: { generate: [gh, completion, -s, \"{{ shell.name }}\"] }\n")
            .with_command("gh completion -s zsh", FakeRun::ok("#compdef gh"));
        let report = apply_on(&h);
        assert!(report.ok(), "{:?}", report.failure);
        assert_eq!(asked(&h, "gh --help"), 0);
        assert!(report.detected.is_empty());
    }

    #[test]
    fn skipping_the_completion_skips_the_question() {
        let h = gh(GH);
        let o = outcome(&h);
        let skip = ["completion/gh".to_string()].into_iter().collect();
        apply::apply(
            &o.plan,
            &o.config,
            &o.facts,
            o.state,
            &h,
            &skip,
            &mut |_: Line| {},
        )
        .unwrap();
        assert_eq!(asked(&h, "gh --help"), 0);

        // And holding the package back holds back its completions.
        let h = gh(GH);
        let o = outcome(&h);
        let skip = ["package/gh".to_string()].into_iter().collect();
        apply::apply(
            &o.plan,
            &o.config,
            &o.facts,
            o.state,
            &h,
            &skip,
            &mut |_: Line| {},
        )
        .unwrap();
        assert_eq!(asked(&h, "gh --help"), 0);
    }

    #[test]
    fn dropping_the_package_takes_the_detected_completion_with_it() {
        let h = gh(GH);
        apply_on(&h);
        let h = h.with_file(
            "/cfg/bedouin.yaml",
            "version: 0\nshell: zsh\npackages: [{name: jq, from: apt}]\n",
        );
        let report = apply_on(&h);
        assert!(report.ok(), "{:?}", report.failure);
        assert!(read(&h, "/home/t/.zshrc.d/completions/_gh").is_none());
    }

    #[test]
    fn opting_out_later_removes_what_was_set_up() {
        let h = gh(GH);
        apply_on(&h);
        assert!(read(&h, "/home/t/.zshrc.d/completions/_gh").is_some());
        let h = h.with_file(
            "/cfg/bedouin.yaml",
            "version: 0\nshell: zsh\npackages: [{name: gh, from: apt, completions: false}]\n",
        );
        apply_on(&h);
        assert!(read(&h, "/home/t/.zshrc.d/completions/_gh").is_none());
    }

    #[test]
    fn a_probe_that_hangs_is_given_up_on_and_the_run_still_succeeds() {
        // The person asked for none of this, so none of it may fail their
        // apply: a generator that never answers is a "no", not an error.
        let h = machine(GH)
            .with_binary("/usr/bin/gh")
            .with_command("gh --help", FakeRun::ok("  completion  Generate"))
            .with_command("gh completion zsh", FakeRun::times_out());
        let report = apply_on(&h);
        assert!(report.ok(), "{:?}", report.failure);
        assert!(report.detected.is_empty());
        // Not a "no": an answer cut short is no answer, so nothing is kept
        // and the next run asks again.
        assert!(
            read(&h, LOCK).is_none_or(|l| !l.contains("\"gh\"")),
            "{:?}",
            read(&h, LOCK)
        );
        apply_on(&h);
        assert_eq!(asked(&h, "gh --help"), 2);
    }

    // ---- from the adversarial review of this feature

    /// gh as above, and also answering for bash, so its form is portable.
    fn gh_both(cfg: &str) -> FakeHost {
        gh(cfg).with_command(
            "gh completion --shell bash",
            FakeRun::ok("# bash completion V2 for gh\ncomplete -o default -F __start_gh gh"),
        )
    }

    #[test]
    fn a_form_is_shared_only_if_it_follows_the_shell() {
        let h = gh_both(GH);
        apply_on(&h);
        assert!(
            lock(&h)["gh"].portable,
            "--shell bash printed bash: the shell is the argument"
        );

        // lazygit-style on a tool that ignores the word: zsh asked, zsh given,
        // but asked for bash it prints zsh again.
        let h = machine("version: 0\nshell: zsh\npackages: [{name: lg, from: apt}]\n")
            .with_binary("/usr/bin/lg")
            .with_command("lg --help", FakeRun::ok("  completion   Generate"))
            .with_command("lg completion zsh", FakeRun::ok("#compdef lg"))
            .with_command("lg completion bash", FakeRun::ok("#compdef lg"));
        apply_on(&h);
        let e = &lock(&h)["lg"];
        assert!(e.generate.is_some(), "it still works on this machine");
        assert!(!e.portable, "but cannot be written as {{{{ shell.name }}}}");
        assert!(read(&h, "/home/t/.zshrc.d/completions/_lg").is_some());
    }

    #[test]
    fn losing_the_lock_does_not_remove_what_it_set_up() {
        let h = gh_both(GH);
        apply_on(&h);
        h.remove(Path::new(LOCK)).unwrap();
        let o = outcome(&h);
        assert!(
            !o.plan
                .items
                .iter()
                .any(|i| i.id == "completion/gh" && i.action == Action::Remove),
            "a cache miss is not an instruction: {:?}",
            o.plan
                .items
                .iter()
                .map(|i| (&i.id, &i.action))
                .collect::<Vec<_>>()
        );
        let report = apply_on(&h);
        assert!(report.ok());
        assert!(read(&h, "/home/t/.zshrc.d/completions/_gh").is_some());
        assert!(
            lock(&h)["gh"].generate.is_some(),
            "and the question is answered again"
        );
    }

    #[test]
    fn a_real_no_after_a_yes_removes_the_file_in_the_same_run() {
        let h = gh_both(GH);
        apply_on(&h);
        assert!(read(&h, "/home/t/.zshrc.d/completions/_gh").is_some());
        // Upgraded, and the new version has no generator at all.
        let h = h
            .with_fingerprint("/usr/bin/gh", "/usr/bin/gh 2 2")
            .with_command("gh --help", FakeRun::ok("Work seamlessly with GitHub."));
        let report = apply_on(&h);
        assert!(report.ok(), "{:?}", report.failure);
        assert!(read(&h, "/home/t/.zshrc.d/completions/_gh").is_none());
        assert_eq!(lock(&h)["gh"].generate, None);
        assert_eq!(outcome(&h).plan.exit_code(), 0, "converged in one pass");
    }

    #[test]
    fn a_held_back_package_holds_back_its_detected_completion() {
        let h = gh_both(GH);
        apply_on(&h);
        // gh gone from the machine and state reset; the lock still remembers
        // its generator, and the plan reinstalls gh, so it plans the
        // completion too.
        let h = FakeHost {
            binaries: vec![PathBuf::from("/usr/bin/apt-get")],
            ..h
        };
        h.remove(Path::new("/home/t/.local/state/bedouin/state.json"))
            .unwrap();
        let o = outcome(&h);
        assert!(o
            .plan
            .items
            .iter()
            .any(|i| i.id == "completion/gh" && i.action != Action::NoOp));
        for skip in ["package/gh", "manager/apt"] {
            let skip = [skip.to_string()].into_iter().collect();
            let o = outcome(&h);
            let report = apply::apply(
                &o.plan,
                &o.config,
                &o.facts,
                o.state,
                &h,
                &skip,
                &mut |_: Line| {},
            )
            .unwrap();
            assert!(report.ok(), "{:?}", report.failure);
            assert!(
                report.skipped.contains(&"completion/gh".to_string()),
                "{:?}",
                report.skipped
            );
        }
    }

    #[test]
    fn a_stale_detected_generator_does_not_stop_the_run() {
        let h = gh_both(GH);
        apply_on(&h);
        // The completion file is lost, so the plan regenerates it from the
        // lock -- but the tool no longer has that command.
        h.remove(Path::new("/home/t/.zshrc.d/completions/_gh"))
            .unwrap();
        let h = h.with_command(
            "gh completion --shell zsh",
            FakeRun::fails(1, "unknown command"),
        );
        let report = apply_on(&h);
        assert!(report.ok(), "{:?}", report.failure);
        assert_eq!(asked(&h, "gh --help"), 2, "and the tool was asked again");
    }

    #[test]
    fn a_shim_is_rechecked_by_its_version() {
        // node behind a mise shim: the file on PATH is mise itself, and it
        // does not change when node does.
        let cfg = "version: 0\nshell: zsh\npackages: [{name: node, from: apt}]\n";
        let base = |v: &str| {
            machine(cfg)
                .with_binary("/usr/bin/node")
                .with_fingerprint("/usr/bin/node", "/home/t/.local/bin/mise 41000 1.0")
                .with_command("node --version", FakeRun::ok(v))
                .with_command("node --help", FakeRun::ok("Usage: node [options]"))
        };
        let h = base("v22.0.0");
        apply_on(&h);
        apply_on(&h);
        assert_eq!(asked(&h, "node --help"), 1, "same version: the lock holds");
        let h = FakeHost {
            commands: base("v24.1.0").commands,
            ..h
        };
        apply_on(&h);
        assert_eq!(
            asked(&h, "node --help"),
            2,
            "new version behind the same shim: asked again"
        );
        assert_eq!(lock(&h)["node"].version.as_deref(), Some("v24.1.0"));
    }

    #[test]
    fn deleting_a_declared_generator_still_removes_its_file() {
        // The keep-alive is for what detection set up. A generator the
        // config declared, whose line is then deleted, goes as it always did
        // -- even for a package detection could never find (ripgrep is `rg`).
        let declared = "version: 0\nshell: zsh\npackages:\n  - name: ripgrep\n    from: apt\n    completions: { generate: [rg, --generate, complete-zsh] }\n";
        let h = machine(declared)
            .with_binary("/usr/bin/ripgrep")
            .with_binary("/usr/bin/rg")
            .with_command("rg --generate complete-zsh", FakeRun::ok("#compdef rg"));
        let r = apply_on(&h);
        assert!(r.ok(), "{:?}", r.failure);
        assert!(read(&h, "/home/t/.zshrc.d/completions/_rg").is_some());
        let h = h.with_file(
            "/cfg/bedouin.yaml",
            "version: 0\nshell: zsh\npackages: [{name: ripgrep, from: apt}]\n",
        );
        let o = outcome(&h);
        assert!(
            o.plan
                .items
                .iter()
                .any(|i| i.id == "completion/ripgrep" && i.action == Action::Remove),
            "{:?}",
            o.plan
                .items
                .iter()
                .map(|i| (&i.id, &i.action))
                .collect::<Vec<_>>()
        );
        apply_on(&h);
        assert!(read(&h, "/home/t/.zshrc.d/completions/_rg").is_none());
    }

    #[test]
    fn a_stale_generator_whose_tool_cannot_be_asked_is_not_retried_forever() {
        let h = gh_both(GH);
        apply_on(&h);
        h.remove(Path::new("/home/t/.zshrc.d/completions/_gh"))
            .unwrap();
        // The generator broke and the tool will not answer --help either.
        let h = h
            .with_command(
                "gh completion --shell zsh",
                FakeRun::fails(1, "unknown command"),
            )
            .with_command("gh --help", FakeRun::times_out());
        let report = apply_on(&h);
        assert!(report.ok(), "{:?}", report.failure);
        let state = run::plan_for(&h, h_path(), Path::new("/cfg"), Os::Linux, Arch::X86_64)
            .unwrap()
            .state;
        assert!(
            state
                .items
                .get("completion/gh")
                .is_none_or(|i| i.status == bedouin_core::state::Status::Complete),
            "not left Incomplete: {:?}",
            state.items.get("completion/gh")
        );
        // The lock's answer is what was wrong, so it is gone, and the plan no
        // longer schedules the broken generator.
        assert!(!lock(&h).contains_key("gh"));
        let o = outcome(&h);
        assert!(!o
            .plan
            .items
            .iter()
            .any(|i| i.id == "completion/gh" && i.action != Action::NoOp));
    }

    #[test]
    fn a_converged_run_that_cannot_save_its_record_says_so() {
        let h = gh_both(
            "version: 0\nshell: zsh\npackages: [{name: gh, from: apt, completions: false}]\n",
        );
        apply_on(&h);
        let h = h
            .with_file("/cfg/bedouin.yaml", "version: 0\nshell: zsh\npackages: [{name: gh, from: apt, completions: false}, {name: jq, from: apt}]\n")
            .with_unwritable("/home/t/.local/state/bedouin/state.json");
        let o = outcome(&h);
        assert!(!o.plan.has_changes());
        let report = apply_on(&h);
        assert!(!report.ok(), "the record was lost and nobody was told");
    }
}
