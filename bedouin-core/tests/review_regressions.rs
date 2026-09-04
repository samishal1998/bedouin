//! Regressions from the M0 adversarial review.
//!
//! Each of these was a shipped defect that the unit tests and the two-machine
//! acceptance test both missed. They are kept together so it is obvious what
//! class of bug the suite was blind to: almost all of them are a *silent wrong
//! answer* rather than a crash.

use bedouin_core::facts::{Arch, Os};
use bedouin_core::host::{FakeHost, FakeRun};
use bedouin_core::run;
use std::path::Path;

fn machine(config: &str) -> FakeHost {
    FakeHost::new()
        .with_file("/cfg/bedouin.yaml", config)
        .with_file("/cfg/templates/x.j2", "hello")
        .with_file(
            "/etc/os-release",
            "ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n",
        )
        .with_env("HOME", "/home/t")
        .with_env("USER", "t")
        .with_env("SHELL", "/bin/zsh")
        .with_env("PATH", "/usr/bin:/bin")
        // Every plan installs bedouin's own completion now, so a fake
        // machine has to be able to answer for it. Both shells, because
        // fixtures here use either.
        .with_env("BEDOUIN_EXE", "bedouin")
        .with_command(
            "bedouin completion-script zsh",
            FakeRun::ok("#compdef bedouin"),
        )
        .with_command("bedouin completion-script bash", FakeRun::ok("# bedouin"))
        .with_binary("/usr/bin/apt-get")
        .with_command("id -u", FakeRun::ok("1000"))
        .with_command("sudo -n true", FakeRun::ok(""))
}

fn plan(config: &str) -> Result<run::Outcome, String> {
    let h = machine(config);
    run::plan_for(
        &h,
        Some(Path::new("/cfg/bedouin.yaml")),
        Path::new("/cfg"),
        Os::Linux,
        Arch::X86_64,
    )
    .map_err(|e| e.to_string())
}

fn err(config: &str) -> String {
    plan(config)
        .err()
        .unwrap_or_else(|| panic!("expected this config to be refused, but it planned cleanly"))
}

#[test]
fn an_absolute_src_cannot_escape_the_config_root() {
    // The containment check exempted anything starting with `/`, so writing the
    // same path absolutely walked straight around it. The executor would then
    // render an arbitrary file into the user's home.
    let e = err("version: 0\npackages: [{name: jq, from: apt}]\nfiles: [{src: /etc/os-release, dest: ~/x}]\n");
    assert!(e.contains("outside the config root"), "{e}");

    // The relative form stays refused, and the legitimate case still works.
    let e = err("version: 0\npackages: [{name: jq, from: apt}]\nfiles: [{src: ../../etc/passwd, dest: ~/x}]\n");
    assert!(e.contains("outside the config root"), "{e}");
    assert!(plan("version: 0\npackages: [{name: jq, from: apt}]\nfiles: [{src: templates/x.j2, dest: ~/x}]\n").is_ok());
}

#[test]
fn an_include_cannot_climb_out_with_dot_dot() {
    // `Path::starts_with` is lexical, so `/cfg/../evil` "starts with" `/cfg`.
    // Both sides are collapsed before the comparison now.
    let h = machine("version: 0\nincludes: [\"../evil/*.yaml\"]\n").with_file(
        "/evil/x.yaml",
        "packages:\n  - {name: outside, from: apt}\n",
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
    assert!(
        e.contains("outside the config root") || e.contains("matches no files"),
        "{e}"
    );
}

#[test]
fn two_items_may_not_share_one_state_key() {
    // rc ids are `rc/{package}/{basename}`, so two blocks in one package whose
    // files share a basename collapsed to one id. With a state file present,
    // both then reported as already done -- including the one never written.
    let e = err(r#"
version: 0
shell: zsh
packages:
  - name: zellij
    from: apt
    rc:
      - { file: "{{ home }}/.zshrc.d/70-z.zsh", content: a }
      - { file: "{{ home }}/other.d/70-z.zsh", content: b }
"#);
    assert!(e.contains("same id"), "{e}");
    assert!(e.contains("70-z.zsh"), "{e}");

    // Two *different* packages writing files of the same basename stays legal --
    // that is exactly why the id carries the package name.
    assert!(plan(
        r#"
version: 0
shell: zsh
packages:
  - name: a
    from: apt
    rc: [{ file: "{{ home }}/.zshrc.d/70-x.zsh", content: a }]
  - name: b
    from: apt
    rc: [{ file: "{{ home }}/.zshrc.d/70-x.zsh", content: b }]
"#
    )
    .is_ok());
}

#[test]
fn an_include_that_matches_nothing_is_refused() {
    // Expanding to nothing in silence is the worst outcome available: every
    // package in the drop-in vanishes, and anything already in state as
    // `owner: bedouin` is then planned for REMOVAL. A typo would read as
    // "uninstall all of this".
    let h = machine("version: 0\nincludes: [conf.d/*.yaml]\npackages: [{name: jq, from: apt}]\n");
    let e = run::plan_for(
        &h,
        Some(Path::new("/cfg/bedouin.yaml")),
        Path::new("/cfg"),
        Os::Linux,
        Arch::X86_64,
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("matches no files"), "{e}");
    assert!(
        e.contains("silently drop"),
        "should say why it matters: {e}"
    );
}

#[test]
fn a_match_value_the_resolver_cannot_produce_is_refused() {
    // `os: darwin` is not an error today -- it is a branch that never matches on
    // any machine, forever. That is the exact failure class the closed arm
    // vocabulary is bought to eliminate, and `match:` was the last place still
    // comparing raw strings.
    let e = err("version: 0\ntargets: [{name: mac, match: {os: darwin}}]\npackages: [{name: jq, from: apt}]\n");
    assert!(e.contains("never match"), "{e}");
    assert!(e.contains("macos"), "names what is valid: {e}");
}

#[test]
fn fact_values_have_one_spelling_everywhere() {
    // `as_str` returned the ARM spelling (`debian-like`) while serde and
    // `bedouin facts` printed the plain one, so `match: {distro_like: debian}`
    // compared "debian" against "debian-like" and silently never matched.
    let o = plan(
        "version: 0\ntargets: [{name: deb, match: {distro_like: debian}}]\n\
         packages: [{name: jq, from: {deb: apt, default: brew}}]\n",
    )
    .expect("plans");
    let jq = o.plan.items.iter().find(|i| i.name == "jq").unwrap();
    assert!(
        jq.detail.contains("apt"),
        "the target must match: {}",
        jq.detail
    );
}

#[test]
fn empty_collections_are_refused_rather_than_silently_doing_nothing() {
    // `only: []` pruned the item on every machine without a word -- and if it
    // was in state as owner:bedouin, proposed to uninstall it.
    let e = err("version: 0\npackages: [{name: jq, from: apt, only: []}]\n");
    assert!(e.contains("names no machine"), "{e}");

    // `from: []` produced a sentence with a hole in it.
    let e = err("version: 0\npackages: [{name: jq, from: []}]\n");
    assert!(e.contains("empty `from:`"), "{e}");
    assert!(!e.contains("asks for ,"), "no degenerate sentence: {e}");
}

#[test]
fn an_interrupted_item_re_diffs_as_needing_work() {
    // §8.3 records intent BEFORE the work so an interrupt cannot mislabel a
    // bedouin package as preexisting. The diff ignored `status`, so a
    // half-installed item planned as a no-op and `plan` exited 0 claiming the
    // machine matched the config.
    let state = r#"{"schema_version":1,"items":{"package/zellij":
        {"kind":"package","owner":"bedouin","status":"incomplete","method":"apt"}}}"#;
    let h = machine("version: 0\npackages: [{name: zellij, from: apt}]\n")
        .with_file("/home/t/.local/state/bedouin/state.json", state);
    let o = run::plan_for(
        &h,
        Some(Path::new("/cfg/bedouin.yaml")),
        Path::new("/cfg"),
        Os::Linux,
        Arch::X86_64,
    )
    .unwrap();
    let z = o.plan.items.iter().find(|i| i.name == "zellij").unwrap();
    assert_ne!(
        z.action,
        bedouin_core::plan::Action::NoOp,
        "a half-installed item must not report as done"
    );
    assert_eq!(o.plan.exit_code(), 2);
}

#[test]
fn ambiguous_arms_are_refused_inside_a_target_vars_block_too() {
    // Every other evaluatable leaf validated before selecting; target `vars:`
    // did not. The same arm pair was a hard error in the base block and a
    // silent fallthrough to `default:` inside a target.
    let e = err(
        "version: 0\ntargets:\n  - name: here\n    match: {distro: ubuntu}\n    \
         vars: {editor: {macos: a, arm64: b, default: c}}\npackages: [{name: jq, from: apt}]\n",
    );
    assert!(e.contains("neither is more specific"), "{e}");
}

#[test]
fn a_long_needs_chain_does_not_overflow_the_stack() {
    // The recursive walker overflowed on a deep chain, and a config is user
    // input. 20 000 is well past anything real and completes in well under a
    // second iteratively.
    let mut cfg = String::from("version: 0\npackages:\n");
    const N: usize = 20_000;
    for i in 0..N {
        cfg.push_str(&format!("  - {{name: p{i}, from: apt"));
        if i + 1 < N {
            cfg.push_str(&format!(", needs: [p{}]", i + 1));
        }
        cfg.push_str("}\n");
    }
    let o = plan(&cfg).expect("a deep chain plans rather than crashing");
    // Count the packages, not every item: every plan also carries bedouin's
    // own completion, and this test is about stack depth on a long chain.
    assert_eq!(
        o.plan
            .items
            .iter()
            .filter(|i| i.id.starts_with("package/"))
            .count(),
        N
    );
    // And the order is still correct: every dependency precedes its dependent.
    let first = o.plan.items.iter().position(|i| i.name == "p0").unwrap();
    let last = o
        .plan
        .items
        .iter()
        .position(|i| i.name == format!("p{}", N - 1))
        .unwrap();
    assert!(last < first, "the deepest prerequisite comes first");
}

#[test]
fn a_needs_cycle_is_reported_rather_than_looping() {
    let e = err(
        "version: 0\npackages:\n  - {name: a, from: apt, needs: [b]}\n  \
         - {name: b, from: apt, needs: [a]}\n",
    );
    assert!(e.contains("cycle"), "{e}");
}
