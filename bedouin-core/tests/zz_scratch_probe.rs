use bedouin_core::apply;
use bedouin_core::facts::{Arch, Os};
use bedouin_core::host::{FakeHost, FakeRun, Host, Line};
use bedouin_core::run;
use std::path::Path;

fn machine(cfg: &str) -> FakeHost {
    FakeHost::new()
        .with_file("/cfg/bedouin.yaml", cfg)
        .with_file("/etc/os-release", "ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n")
        .with_env("HOME", "/home/t").with_env("USER", "t")
        .with_env("SHELL", "/bin/zsh").with_env("PATH", "/usr/bin:/bin")
        .with_binary("/usr/bin/apt-get")
        .with_command("id -u", FakeRun::ok("1000"))
        .with_command("sudo -n true", FakeRun::ok(""))
        .with_command("sudo -n apt-get update", FakeRun::ok(""))
        .with_command("sudo -n apt-get install -y kubectl", FakeRun::ok("ok"))
        .with_command("sudo -n apt-get remove -y kubectl", FakeRun::ok("ok"))
        .with_command("sudo -n apt-get purge -y kubectl", FakeRun::ok("ok"))
        .with_binary("/usr/bin/jq").with_command("kubectl completion zsh", FakeRun::ok("#compdef kubectl\n_k() { :; }"))
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

const FULL: &str = r#"
version: 0
shell: zsh
aliases: { ll: ls -alh }
packages:
  - name: kubectl
    from: apt
    path: ["~/.krew/bin"]
    aliases: { k: kubectl }
    completions: { generate: ["kubectl", "completion", "{{ shell.name }}"] }
    rc:
      - file: "{{ shell.rc_dir }}/70-kubectl.zsh"
        content: "export KUBE_EDITOR=vi"
"#;

#[test]
fn everything_at_once_then_remove() {
    let h = machine(FULL);
    let r = apply_on(&h);
    println!("apply1 ok={} completed={:?} fail={:?}", r.ok(), r.completed, r.failure);
    for f in ["/home/t/.zshrc", "/home/t/.zshrc.d/10-bedouin-aliases.zsh",
              "/home/t/.zshrc.d/30-kubectl-aliases.zsh", "/home/t/.zshrc.d/70-kubectl.zsh",
              "/home/t/.zshrc.d/00-bedouin-path.zsh", "/home/t/.zshrc.d/completions/_kubectl"] {
        println!("  {f} => {:?}", read(&h, f));
    }
    println!("plan2 exit={} (idempotent?)", outcome(&h).plan.exit_code());
    let o = outcome(&h);
    println!("doctor exit={}", bedouin_core::doctor::check(&o.state, &o.config, &o.facts, &h).unwrap().exit_code());

    // now drop the package entirely
    let h2 = h.with_file("/cfg/bedouin.yaml", "version: 0\nshell: zsh\naliases: { ll: ls -alh }\npackages: [{name: jq, from: apt}]\n");
    let r = apply_on(&h2);
    println!("apply-remove ok={} completed={:?} fail={:?}", r.ok(), r.completed, r.failure);
    for f in ["/home/t/.zshrc.d/30-kubectl-aliases.zsh", "/home/t/.zshrc.d/70-kubectl.zsh",
              "/home/t/.zshrc.d/00-bedouin-path.zsh", "/home/t/.zshrc.d/completions/_kubectl",
              "/home/t/.zshrc.d/10-bedouin-aliases.zsh"] {
        println!("  {f} => {:?}", read(&h2, f));
    }
    println!("plan3 exit={}", outcome(&h2).plan.exit_code());
    let o = outcome(&h2);
    let rep3 = bedouin_core::doctor::check(&o.state, &o.config, &o.facts, &h2).unwrap();
    println!("doctor3 exit={}\n{}", rep3.exit_code(), rep3.render(true));
}

#[test]
fn rc_block_into_the_users_own_zshrc_and_removal() {
    let cfg = r#"
version: 0
shell: zsh
packages:
  - name: kubectl
    from: apt
    rc:
      - file: "~/.zshrc"
        content: "export KUBE_EDITOR=vi"
"#;
    let h = machine(cfg).with_file("/home/t/.zshrc", "# my own zshrc\nexport EDITOR=vim\n");
    let r = apply_on(&h);
    println!("ok={} fail={:?}", r.ok(), r.failure);
    println!("zshrc:\n{}", read(&h, "/home/t/.zshrc").unwrap());
    let h2 = h.with_file("/cfg/bedouin.yaml", "version: 0\nshell: zsh\npackages: [{name: jq, from: apt}]\n");
    apply_on(&h2);
    println!("zshrc after removal:\n{:?}", read(&h2, "/home/t/.zshrc"));
}
