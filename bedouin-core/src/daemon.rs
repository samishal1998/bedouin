//! Unit files for running `bedouin reconcile --watch` unattended.
//!
//! Bedouin generates the unit and hands it to the platform's own supervisor;
//! it does not become one. Starting, stopping and logging are systemd's and
//! launchd's job, they already do it better, and a config tool that also
//! supervises processes is two tools.

use crate::facts::{Facts, Os};
use std::path::PathBuf;

/// Where the unit belongs, and what it should contain.
pub struct Unit {
    pub path: PathBuf,
    pub contents: String,
    /// What the user runs to make the supervisor pick it up. Printed rather
    /// than executed: enabling a background service is the user's call.
    pub enable: Vec<String>,
}

pub fn unit_for(facts: &Facts, exe: &str, config: &str, interval_secs: u64) -> Unit {
    match facts.os {
        Os::Macos => {
            let label = "dev.bedouin.reconcile";
            Unit {
                path: facts
                    .home
                    .join(format!("Library/LaunchAgents/{label}.plist")),
                contents: format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>--config</string><string>{config}</string>
    <string>reconcile</string>
  </array>
  <key>StartInterval</key><integer>{interval_secs}</integer>
  <key>RunAtLoad</key><false/>
  <key>StandardOutPath</key><string>{home}/.local/state/bedouin/reconcile.log</string>
  <key>StandardErrorPath</key><string>{home}/.local/state/bedouin/reconcile.log</string>
</dict>
</plist>
"#,
                    home = facts.home.display()
                ),
                enable: vec![format!(
                    "launchctl load -w ~/Library/LaunchAgents/{label}.plist"
                )],
            }
        }
        Os::Linux => Unit {
            path: facts.home.join(".config/systemd/user/bedouin-reconcile.timer"),
            contents: format!(
                "# Written by `bedouin daemon install`.\n\
                 # The service unit sits beside this file.\n\
                 [Unit]\n\
                 Description=Reconcile this machine with bedouin.yaml\n\n\
                 [Timer]\n\
                 OnBootSec={interval_secs}s\n\
                 OnUnitActiveSec={interval_secs}s\n\
                 Persistent=true\n\n\
                 [Install]\n\
                 WantedBy=timers.target\n"
            ),
            enable: vec![
                "systemctl --user daemon-reload".into(),
                "systemctl --user enable --now bedouin-reconcile.timer".into(),
            ],
        },
    }
}

/// The service unit a systemd timer triggers. `None` on macOS, where the
/// plist carries both halves.
pub fn service_for(facts: &Facts, exe: &str, config: &str) -> Option<Unit> {
    if facts.os != Os::Linux {
        return None;
    }
    Some(Unit {
        path: facts
            .home
            .join(".config/systemd/user/bedouin-reconcile.service"),
        contents: format!(
            "# Written by `bedouin daemon install`.\n\
             [Unit]\n\
             Description=Reconcile this machine with bedouin.yaml\n\n\
             [Service]\n\
             Type=oneshot\n\
             ExecStart={exe} --config {config} reconcile\n"
        ),
        enable: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Arch, Distro};

    #[test]
    fn a_linux_install_is_a_timer_plus_a_oneshot_service() {
        let f = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
        let t = unit_for(&f, "/usr/local/bin/bedouin", "/cfg/bedouin.yaml", 900);
        assert!(t.path.ends_with(".config/systemd/user/bedouin-reconcile.timer"));
        assert!(t.contents.contains("OnUnitActiveSec=900s"));
        // Persistent, so a laptop that was asleep still reconciles on waking.
        assert!(t.contents.contains("Persistent=true"));

        let s = service_for(&f, "/usr/local/bin/bedouin", "/cfg/bedouin.yaml").unwrap();
        assert!(s.contents.contains("Type=oneshot"));
        assert!(s.contents.contains("ExecStart=/usr/local/bin/bedouin --config /cfg/bedouin.yaml reconcile"));
        assert!(t.enable.iter().any(|c| c.contains("--user enable")));
    }

    #[test]
    fn a_macos_install_is_one_launch_agent() {
        let f = Facts::fixture(Os::Macos, Distro::Macos, Arch::Arm64);
        let u = unit_for(&f, "/usr/local/bin/bedouin", "/cfg/bedouin.yaml", 900);
        assert!(u.path.ends_with("Library/LaunchAgents/dev.bedouin.reconcile.plist"));
        assert!(u.contents.contains("<key>StartInterval</key><integer>900</integer>"));
        // Not at login: a reconcile that runs while the machine is still
        // coming up is a reconcile against a half-ready machine.
        assert!(u.contents.contains("<key>RunAtLoad</key><false/>"));
        assert!(service_for(&f, "x", "y").is_none(), "launchd needs no second unit");
    }

    #[test]
    fn the_command_that_enables_it_is_printed_not_run() {
        // Enabling a background service that mutates the machine is the user's
        // decision, so `daemon install` writes the file and says what to run.
        let f = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
        let t = unit_for(&f, "bedouin", "/cfg/bedouin.yaml", 60);
        assert!(!t.enable.is_empty());
    }
}
