use bedouin_core::apply;
use bedouin_core::facts::{Arch, Os};
use bedouin_core::host::{FakeHost, FakeRun, Host, Line};
use bedouin_core::run;
use std::path::Path;

fn machine(cfg: &str) -> FakeHost {
    FakeHost::new()
        .with_file("/cfg/bedouin.yaml", cfg)
        .with_file("/etc/os-release", "ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n")
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
    run::plan_for(h, Some(Path::new("/cfg/bedouin.yaml")), Path::new("/cfg"), Os::Linux, Arch::X86_64)
        .unwrap_or_else(|e| panic!("plan failed: {e}"))
}
fn apply_on(h: &FakeHost) -> apply::Report {
    let o = outcome(h);
    apply::apply(&o.plan, &o.config, &o.facts, o.state, h, &mut |_: Line| {}).unwrap()
}
fn read(h: &FakeHost, p: &str) -> Option<String> {
    h.read(Path::new(p)).unwrap().map(|b| String::from_utf8_lossy(&b).into_owned())
}

const CFG: &str = r#"
version: 0
shell: zsh
aliases:
  ll: ls -alh
packages:
  - name: jq
    from: apt
    completions:
      generate: ["jq", "--completion", "{{ shell.name }}"]
"#;

fn fresh() -> FakeHost {
    machine(CFG).with_command("jq --completion zsh", FakeRun::ok("#compdef jq\n_jq() { :; }"))
}

#[test]
fn probe1_completions_hand_edit() {
    let h = fresh();
    let r = apply_on(&h);
    assert!(r.ok(), "{:?}", r.failure);
    let f = "/home/t/.zshrc.d/completions/_jq";
    println!("written: {:?}", read(&h, f));
    // hand-edit the completions file
    h.files.borrow_mut().insert(f.into(), b"# someone broke this\n".to_vec());

    let o = outcome(&h);
    let rep = bedouin_core::doctor::check(&o.state, &o.config, &o.facts, &h).unwrap();
    println!("DOCTOR exit={} clean={}\n{}", rep.exit_code(), rep.is_clean(), rep.render(false));
    println!("PLAN exit={} \n{}", o.plan.exit_code(), o.plan.render(false));
    // now re-apply and re-check
    let r2 = apply_on(&h);
    println!("apply2 completed={:?} ok={}", r2.completed, r2.ok());
    println!("file after apply2: {:?}", read(&h, f));
    let o = outcome(&h);
    let rep = bedouin_core::doctor::check(&o.state, &o.config, &o.facts, &h).unwrap();
    println!("DOCTOR AGAIN exit={}\n{}", rep.exit_code(), rep.render(false));
}

#[test]
fn probe3a_doctor_after_failed_apply() {
    let h = machine(CFG).with_command("jq --completion zsh", FakeRun::fails(2, "nope"));
    let r = apply_on(&h);
    println!("apply failed at: {:?}", r.failure.as_ref().map(|f| f.id.clone()));
    let o = outcome(&h);
    let rep = bedouin_core::doctor::check(&o.state, &o.config, &o.facts, &h).unwrap();
    println!("DOCTOR exit={}\n{}", rep.exit_code(), rep.render(false));
    println!("PLAN exit={}\n{}", o.plan.exit_code(), o.plan.render(false));
    println!("state: {}", serde_json::to_string_pretty(&o.state).unwrap());
}

#[test]
fn probe3b_package_uninstalled_by_hand() {
    let cfg = "version: 0\nshell: zsh\npackages:\n  - name: ripgrep\n    from: apt\n";
    let h = machine(cfg).with_command("sudo -n apt-get update", FakeRun::ok(""))
        .with_command("sudo -n apt-get install -y ripgrep", FakeRun::ok("done"));
    let r = apply_on(&h);
    println!("apply ok={} completed={:?} fail={:?}", r.ok(), r.completed, r.failure);
    // uninstall by hand: the binary is not on the machine (never was in FakeHost)
    let o = outcome(&h);
    let rep = bedouin_core::doctor::check(&o.state, &o.config, &o.facts, &h).unwrap();
    println!("DOCTOR exit={}\n{}", rep.exit_code(), rep.render(false));
    println!("PLAN exit={}\n{}", o.plan.exit_code(), o.plan.render(false));
}

#[test]
fn probe11_package_named_bedouin() {
    let cfg = "version: 0\nshell: zsh\naliases:\n  ll: ls -alh\npackages:\n  - name: bedouin\n    from: apt\n    aliases:\n      b: bedouin\n";
    let h = machine(cfg);
    match run::plan_for(&h, Some(Path::new("/cfg/bedouin.yaml")), Path::new("/cfg"), Os::Linux, Arch::X86_64) {
        Ok(o) => {
            println!("PLAN OK:\n{}", o.plan.render(true));
            for i in &o.plan.items { println!("  id={} name={}", i.id, i.name); }
        }
        Err(e) => println!("PLAN ERR: {}", e.message),
    }
}
