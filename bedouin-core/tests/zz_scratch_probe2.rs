use bedouin_core::apply;
use bedouin_core::facts::{Arch, Os};
use bedouin_core::host::{FakeHost, FakeRun, Host, Line};
use bedouin_core::run;
use std::path::Path;

fn machine(cfg: &str, osrel: &str) -> FakeHost {
    FakeHost::new()
        .with_file("/cfg/bedouin.yaml", cfg)
        .with_file("/etc/os-release", osrel)
        .with_env("HOME", "/home/t").with_env("USER", "t")
        .with_env("SHELL", "/bin/zsh").with_env("PATH", "/usr/bin:/bin")
        .with_binary("/usr/bin/apt-get").with_binary("/usr/bin/zypper")
        .with_command("id -u", FakeRun::ok("1000"))
        .with_command("sudo -n true", FakeRun::ok(""))
        .with_command("sudo -n apt-get update", FakeRun::ok(""))
        .with_command("sudo -n apt-get install -y ripgrep=1.0", FakeRun::ok("ok"))
        .with_command("sudo -n apt-get install -y ripgrep", FakeRun::ok("ok"))
        .with_command("sudo -n zypper --non-interactive install ripgrep", FakeRun::ok("ok"))
        .with_command("sudo -n zypper --non-interactive install ripgrep=2.0", FakeRun::ok("ok"))
        .with_command("sudo -n zypper --non-interactive refresh", FakeRun::ok(""))
}
const UBUNTU: &str = "ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n";
const SUSE: &str = "ID=opensuse\nID_LIKE=suse\nVERSION_ID=\"20260101\"\n";
const CFG: &str = r#"
version: 0
shell: zsh
packages:
  - name: ripgrep
    from:
      ubuntu: apt
      opensuse: zypper
    version:
      ubuntu: "1.0"
      opensuse: "2.0"
"#;
fn out(h: &FakeHost) -> run::Outcome {
    run::plan_for(h, Some(Path::new("/cfg/bedouin.yaml")), Path::new("/cfg"), Os::Linux, Arch::X86_64).unwrap()
}
#[test]
fn resolved_from_roundtrip() {
    let h = machine(CFG, UBUNTU);
    let o = out(&h);
    let r = apply::apply(&o.plan, &o.config, &o.facts, o.state, &h, &mut |_: Line| {}).unwrap();
    println!("apply ok={} fail={:?}", r.ok(), r.failure);
    let st = h.read(Path::new("/home/t/.local/state/bedouin/state.json")).unwrap().unwrap();
    println!("state: {}", String::from_utf8_lossy(&st));
    let o = out(&h);
    let rep = bedouin_core::doctor::check(&o.state, &o.config, &o.facts, &h).unwrap();
    println!("SAME MACHINE doctor exit={}\n{}", rep.exit_code(), rep.render(false));

    // the machine "moved": same state file, now opensuse
    let h2 = machine(CFG, SUSE)
        .with_file("/home/t/.local/state/bedouin/state.json", &String::from_utf8_lossy(&st));
    let o2 = out(&h2);
    let rep2 = bedouin_core::doctor::check(&o2.state, &o2.config, &o2.facts, &h2).unwrap();
    println!("MOVED doctor exit={}\n{}", rep2.exit_code(), rep2.render(false));
    println!("MOVED plan exit={}\n{}", o2.plan.exit_code(), o2.plan.render(true));
}
