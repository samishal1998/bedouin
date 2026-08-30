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
    run::plan_for(h, Some(Path::new("/cfg/bedouin.yaml")), Path::new("/cfg"), Os::Linux, Arch::X86_64).unwrap()
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
packages:
  - name: jq
    from: apt
    completions:
      generate: ["jq", "--completion", "{{ shell.name }}"]
"#;

#[test]
fn repro() {
    let h = machine(CFG).with_command("jq --completion zsh", FakeRun::ok("#compdef jq\n_jq() { :; }"));
    apply_on(&h);
    let f = "/home/t/.zshrc.d/completions/_jq";
    println!("after apply: {:?}", read(&h, f));
    h.files.borrow_mut().insert(f.into(), b"# someone broke this".to_vec());

    let o = outcome(&h);
    let d = bedouin_core::doctor::check(&o.state, &o.config, &o.facts, &h).unwrap();
    println!("doctor exit={} clean={}", d.exit_code(), d.is_clean());
    println!("{}", d.render(false));
    println!("plan exit={}", o.plan.exit_code());
    let it = o.plan.items.iter().find(|i| i.id == "completion/jq").unwrap();
    println!("plan action = {:?}", it.action);
    let rep = apply_on(&h);
    println!("apply ok={} changed={:?}", rep.ok(), rep.completed.len());
    println!("file now: {:?}", read(&h, f));
    let o2 = outcome(&h);
    let d2 = bedouin_core::doctor::check(&o2.state, &o2.config, &o2.facts, &h).unwrap();
    println!("doctor again exit={}", d2.exit_code());
}
