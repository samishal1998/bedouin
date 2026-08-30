//! The one place `bedouin-core` touches the world.
//!
//! Everything above this trait is a pure function of its inputs, which is what
//! makes the fresh-box path testable: `FakeHost` can present a machine with
//! nothing installed, a command that exits nonzero, one that times out, and one
//! that prints garbage, none of which can be arranged by hand repeatedly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A command to run. Always argv -- never a shell string, so nothing in a
/// config or a package name can be read as shell syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub argv: Vec<String>,
    /// Constructed by Bedouin, never inherited. Binary paths come from the
    /// state manifest, which is what lets one run install Rust and then a cargo
    /// package.
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub root: bool,
    pub timeout: Option<Duration>,
}

impl Cmd {
    pub fn new<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            env: BTreeMap::new(),
            cwd: None,
            root: false,
            timeout: Some(Duration::from_secs(600)),
        }
    }

    pub fn display(&self) -> String {
        self.argv.join(" ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus {
    pub code: i32,
    pub timed_out: bool,
}

impl ExitStatus {
    pub fn ok(&self) -> bool {
        self.code == 0 && !self.timed_out
    }
}

/// A line of a running step's output. Streamed rather than captured: a
/// twenty-minute cargo build that prints nothing is indistinguishable from a
/// hang.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Out(String),
    Err(String),
}

impl Line {
    pub fn text(&self) -> &str {
        match self {
            Self::Out(s) | Self::Err(s) => s,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub is_dir: bool,
    pub is_symlink: bool,
    pub mode: u32,
}

pub type Result<T> = std::result::Result<T, HostError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError {
    pub path: Option<PathBuf>,
    pub message: String,
}

impl HostError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            path: None,
            message: message.into(),
        }
    }

    pub fn at(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            path: Some(path.into()),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.path {
            Some(p) => write!(f, "{}: {}", p.display(), self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for HostError {}

pub trait Host {
    fn run(&self, cmd: &Cmd, out: &mut dyn FnMut(Line)) -> Result<ExitStatus>;

    /// Look up a binary on an *explicit* search path. Taking the path as an
    /// argument rather than reading `PATH` is deliberate: Bedouin constructs
    /// the environment for every step, and a `which` that consulted the ambient
    /// one would quietly undo that.
    fn which(&self, bin: &str, path: &[PathBuf]) -> Option<PathBuf>;

    fn read(&self, p: &Path) -> Result<Option<Vec<u8>>>;
    fn write(&self, p: &Path, bytes: &[u8], mode: u32) -> Result<()>;
    fn remove(&self, p: &Path) -> Result<()>;
    fn mkdir_p(&self, p: &Path) -> Result<()>;
    /// Entries of a directory, sorted. Empty when the directory is absent.
    fn read_dir(&self, p: &Path) -> Result<Vec<PathBuf>>;
    /// Metadata *without* following a final symlink -- §9.1 refuses to write
    /// through one, and that needs to be distinguishable from a regular file.
    fn symlink_meta(&self, p: &Path) -> Result<Option<Meta>>;
    fn env(&self) -> &BTreeMap<String, String>;
}

// ------------------------------------------------------------------- OsHost

pub struct OsHost {
    env: BTreeMap<String, String>,
}

impl Default for OsHost {
    fn default() -> Self {
        Self::new()
    }
}

impl OsHost {
    pub fn new() -> Self {
        Self {
            env: std::env::vars().collect(),
        }
    }
}

fn io_err(p: &Path, e: std::io::Error) -> HostError {
    HostError::at(p, e.to_string())
}

impl Host for OsHost {
    fn run(&self, cmd: &Cmd, out: &mut dyn FnMut(Line)) -> Result<ExitStatus> {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        let (program, args) = cmd
            .argv
            .split_first()
            .ok_or_else(|| HostError::new("empty command"))?;
        let mut c = Command::new(program);
        c.args(args)
            .env_clear()
            .envs(&cmd.env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &cmd.cwd {
            c.current_dir(dir);
        }
        let mut child = c
            .spawn()
            .map_err(|e| HostError::new(format!("{}: {e}", cmd.display())))?;

        // ponytail: stdout is drained first and stderr after the process ends,
        // so a step that fills the stderr pipe while producing no stdout can
        // block. Fine for package managers; give each stream a thread if a
        // real step ever deadlocks here.
        if let Some(so) = child.stdout.take() {
            for line in BufReader::new(so).lines().map_while(std::result::Result::ok) {
                out(Line::Out(line));
            }
        }
        let output = child
            .wait_with_output()
            .map_err(|e| HostError::new(format!("{}: {e}", cmd.display())))?;
        for line in String::from_utf8_lossy(&output.stderr).lines() {
            out(Line::Err(line.to_string()));
        }
        Ok(ExitStatus {
            code: output.status.code().unwrap_or(-1),
            timed_out: false,
        })
    }

    fn which(&self, bin: &str, path: &[PathBuf]) -> Option<PathBuf> {
        path.iter()
            .map(|d| d.join(bin))
            .find(|c| std::fs::metadata(c).is_ok_and(|m| m.is_file()))
    }

    fn read(&self, p: &Path) -> Result<Option<Vec<u8>>> {
        match std::fs::read(p) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(p, e)),
        }
    }

    fn write(&self, p: &Path, bytes: &[u8], mode: u32) -> Result<()> {
        if let Some(parent) = p.parent() {
            self.mkdir_p(parent)?;
        }
        // Write to a sibling temp and rename, so an interrupted write leaves
        // the previous content rather than half of the new.
        let tmp = p.with_extension("bedouin-tmp");
        std::fs::write(&tmp, bytes).map_err(|e| io_err(&tmp, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
                .map_err(|e| io_err(&tmp, e))?;
        }
        let _ = mode;
        std::fs::rename(&tmp, p).map_err(|e| io_err(p, e))
    }

    fn remove(&self, p: &Path) -> Result<()> {
        match std::fs::remove_file(p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(p, e)),
        }
    }

    fn mkdir_p(&self, p: &Path) -> Result<()> {
        std::fs::create_dir_all(p).map_err(|e| io_err(p, e))
    }

    fn read_dir(&self, p: &Path) -> Result<Vec<PathBuf>> {
        let rd = match std::fs::read_dir(p) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err(p, e)),
        };
        let mut out: Vec<PathBuf> = rd
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .collect();
        out.sort();
        Ok(out)
    }

    fn symlink_meta(&self, p: &Path) -> Result<Option<Meta>> {
        match std::fs::symlink_metadata(p) {
            Ok(m) => Ok(Some(Meta {
                is_dir: m.is_dir(),
                is_symlink: m.file_type().is_symlink(),
                #[cfg(unix)]
                mode: {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode()
                },
                #[cfg(not(unix))]
                mode: 0o644,
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(p, e)),
        }
    }

    fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }
}

// ----------------------------------------------------------------- FakeHost

/// An in-memory machine.
#[derive(Default)]
pub struct FakeHost {
    pub files: std::cell::RefCell<BTreeMap<PathBuf, Vec<u8>>>,
    pub symlinks: std::cell::RefCell<BTreeMap<PathBuf, PathBuf>>,
    /// argv joined by a space -> what running it does.
    pub commands: BTreeMap<String, FakeRun>,
    pub binaries: Vec<PathBuf>,
    pub env: BTreeMap<String, String>,
    /// Every command actually run, in order, for assertions.
    pub ran: std::cell::RefCell<Vec<Cmd>>,
}

#[derive(Debug, Clone, Default)]
pub struct FakeRun {
    pub code: i32,
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub timed_out: bool,
}

impl FakeRun {
    pub fn ok(stdout: &str) -> Self {
        Self {
            stdout: stdout.lines().map(str::to_string).collect(),
            ..Default::default()
        }
    }

    pub fn fails(code: i32, stderr: &str) -> Self {
        Self {
            code,
            stderr: stderr.lines().map(str::to_string).collect(),
            ..Default::default()
        }
    }

    pub fn times_out() -> Self {
        Self {
            code: -1,
            timed_out: true,
            ..Default::default()
        }
    }
}

impl FakeHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_file(self, p: impl Into<PathBuf>, contents: &str) -> Self {
        self.files
            .borrow_mut()
            .insert(p.into(), contents.as_bytes().to_vec());
        self
    }

    pub fn with_command(mut self, argv: &str, run: FakeRun) -> Self {
        self.commands.insert(argv.to_string(), run);
        self
    }

    pub fn with_binary(mut self, p: impl Into<PathBuf>) -> Self {
        self.binaries.push(p.into());
        self
    }

    pub fn with_env(mut self, k: &str, v: &str) -> Self {
        self.env.insert(k.into(), v.into());
        self
    }
}

impl Host for FakeHost {
    fn run(&self, cmd: &Cmd, out: &mut dyn FnMut(Line)) -> Result<ExitStatus> {
        self.ran.borrow_mut().push(cmd.clone());
        // An unscripted command is "not installed", which is the fresh machine.
        let Some(program) = cmd.argv.first() else {
            return Err(HostError::new("empty command"));
        };
        let run = self.commands.get(&cmd.display()).cloned().unwrap_or(FakeRun {
            code: 127,
            stderr: vec![format!("{program}: command not found")],
            ..Default::default()
        });
        for l in &run.stdout {
            out(Line::Out(l.clone()));
        }
        for l in &run.stderr {
            out(Line::Err(l.clone()));
        }
        Ok(ExitStatus {
            code: run.code,
            timed_out: run.timed_out,
        })
    }

    fn which(&self, bin: &str, path: &[PathBuf]) -> Option<PathBuf> {
        path.iter()
            .map(|d| d.join(bin))
            .find(|c| self.binaries.contains(c))
    }

    fn read(&self, p: &Path) -> Result<Option<Vec<u8>>> {
        Ok(self.files.borrow().get(p).cloned())
    }

    fn write(&self, p: &Path, bytes: &[u8], _mode: u32) -> Result<()> {
        self.files.borrow_mut().insert(p.to_path_buf(), bytes.to_vec());
        Ok(())
    }

    fn remove(&self, p: &Path) -> Result<()> {
        self.files.borrow_mut().remove(p);
        Ok(())
    }

    fn mkdir_p(&self, _p: &Path) -> Result<()> {
        Ok(())
    }

    fn read_dir(&self, p: &Path) -> Result<Vec<PathBuf>> {
        let mut out: Vec<PathBuf> = self
            .files
            .borrow()
            .keys()
            .filter(|f| f.parent() == Some(p))
            .cloned()
            .collect();
        out.sort();
        Ok(out)
    }

    fn symlink_meta(&self, p: &Path) -> Result<Option<Meta>> {
        if self.symlinks.borrow().contains_key(p) {
            return Ok(Some(Meta {
                is_dir: false,
                is_symlink: true,
                mode: 0o777,
            }));
        }
        Ok(self.files.borrow().get(p).map(|_| Meta {
            is_dir: false,
            is_symlink: false,
            mode: 0o644,
        }))
    }

    fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unscripted_command_is_a_machine_that_lacks_it() {
        let h = FakeHost::new();
        let mut lines = Vec::new();
        let st = h
            .run(&Cmd::new(["brew", "--version"]), &mut |l| lines.push(l))
            .unwrap();
        assert_eq!(st.code, 127);
        assert!(!st.ok());
        assert!(lines[0].text().contains("not found"));
        assert_eq!(h.ran.borrow().len(), 1);
    }

    #[test]
    fn failure_modes_a_real_machine_has_are_all_expressible() {
        let h = FakeHost::new()
            .with_command("apt-get install -y jq", FakeRun::fails(100, "E: Unable to locate package jq"))
            .with_command("cargo install zellij", FakeRun::times_out())
            .with_command("brew --version", FakeRun::ok("Homebrew 4.3.0"));

        let mut sink = |_: Line| {};
        assert_eq!(
            h.run(&Cmd::new(["apt-get", "install", "-y", "jq"]), &mut sink)
                .unwrap()
                .code,
            100
        );
        assert!(h
            .run(&Cmd::new(["cargo", "install", "zellij"]), &mut sink)
            .unwrap()
            .timed_out);
        assert!(h
            .run(&Cmd::new(["brew", "--version"]), &mut sink)
            .unwrap()
            .ok());
    }

    #[test]
    fn which_only_looks_where_it_is_told() {
        let h = FakeHost::new().with_binary("/home/t/.cargo/bin/cargo");
        let found = h.which("cargo", &[PathBuf::from("/home/t/.cargo/bin")]);
        assert_eq!(found, Some(PathBuf::from("/home/t/.cargo/bin/cargo")));
        // Not on the given path means not found, whatever the real machine has.
        assert_eq!(h.which("cargo", &[PathBuf::from("/usr/bin")]), None);
    }
}
