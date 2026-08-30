//! `.env.bedouin`: the variables a config reads, and where they come from.
//!
//! §7.3 already scans the raw config for referenced variables so the plan
//! artifact can freeze them. This exposes that scan, and gives the scaffold it
//! writes a reader — a file the tool creates and never consumes is a trap.

use crate::host::Host;
use crate::schema::{ConfigError, RawConfig, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = ".env.bedouin";

pub fn path_beside(config_root: &Path) -> PathBuf {
    config_root.join(FILE_NAME)
}

/// One variable the config reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Referenced {
    pub name: String,
    /// Where it is read from, in config terms.
    pub site: String,
    pub set: bool,
    /// Whether every reference guards it with `| default(...)`. An unset
    /// variable with no default is a resolve-time failure waiting to happen.
    pub has_default: bool,
}

/// `KEY=value` lines. Blank lines and `#` comments are skipped; a value may be
/// quoted. Deliberately not a shell parser -- no expansion, no `export`
/// semantics, nothing that would make the file's meaning depend on a shell.
pub fn parse(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        if k.is_empty() {
            continue;
        }
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(v);
        out.insert(k.to_string(), v.to_string());
    }
    out
}

/// Load `.env.bedouin` beside the config, if it is there.
///
/// The process environment wins on a collision: what you exported for this
/// command is more specific than what the file says in general.
pub fn load(host: &dyn Host, config_root: &Path) -> Result<BTreeMap<String, String>> {
    let p = path_beside(config_root);
    let Some(bytes) = host.read(&p).map_err(|e| ConfigError::new(e.to_string()))? else {
        return Ok(BTreeMap::new());
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| ConfigError::new(format!("{}: not valid UTF-8", p.display())))?;
    Ok(parse(&text))
}

/// Every variable the config reads, with where and whether it is set.
pub fn referenced(raw: &RawConfig, env: &BTreeMap<String, String>) -> Vec<Referenced> {
    let sites = sites(raw);
    crate::artifact::referenced_env(raw)
        .into_iter()
        .map(|name| {
            let (site, has_default) = sites
                .get(&name)
                .cloned()
                .unwrap_or_else(|| ("(unknown)".into(), false));
            Referenced {
                // An empty value is the scaffold's own placeholder, not a
                // setting -- counting it as set would report a config as ready
                // when the line was never filled in.
                set: env.get(&name).is_some_and(|v| !v.is_empty()),
                name,
                site,
                has_default,
            }
        })
        .collect()
}

/// Where each variable is read, and whether every read guards it.
fn sites(raw: &RawConfig) -> BTreeMap<String, (String, bool)> {
    let mut out: BTreeMap<String, (String, bool)> = BTreeMap::new();
    let note = &mut |name: &str, site: String, guarded: bool| {
        out.entry(name.to_string())
            .and_modify(|(_, g)| *g = *g && guarded)
            .or_insert((site, guarded));
    };

    for t in &raw.targets {
        if let Some(env) = &t.r#match.env {
            for k in env.keys() {
                // A `match:` on an unset variable is not a failure; it simply
                // does not match.
                note(k, format!("targets.{}", t.name), true);
            }
        }
    }

    let scan = |text: &str, site: &str, note: &mut dyn FnMut(&str, String, bool)| {
        let mut rest = text;
        while let Some(i) = rest.find("env.") {
            let after = &rest[i + 4..];
            let name: String = after
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                // `{{ env.X | default('y') }}` cannot fail; a bare one can.
                let tail: String = after.chars().skip(name.len()).take(40).collect();
                note(&name, site.to_string(), tail.contains("default"));
            }
            rest = &rest[i + 4..];
        }
    };

    for (k, v) in &raw.vars {
        for t in v.payloads() {
            scan(t.as_str(), &format!("vars.{k}"), note);
        }
    }
    for (k, v) in &raw.aliases {
        for t in v.payloads() {
            scan(t.as_str(), &format!("aliases.{k}"), note);
        }
    }
    for p in &raw.packages {
        let site = format!("packages.{}", p.name);
        for v in p.version.iter() {
            for t in v.payloads() {
                scan(t.as_str(), &site, note);
            }
        }
        for b in &p.rc {
            for t in b.content.payloads() {
                scan(t.as_str(), &site, note);
            }
            for t in b.file.payloads() {
                scan(t.as_str(), &site, note);
            }
        }
        if let Some(path) = &p.path {
            for many in path.payloads() {
                for t in many.iter() {
                    scan(t.as_str(), &site, note);
                }
            }
        }
    }
    for f in &raw.files {
        let hint = f
            .dest
            .payloads()
            .next()
            .map(|t| t.as_str().to_string())
            .unwrap_or_default();
        for v in [&f.src, &f.dest] {
            for t in v.payloads() {
                scan(t.as_str(), &format!("files.{hint}"), note);
            }
        }
    }
    out
}

/// The scaffold `--write` produces.
pub fn scaffold(refs: &[Referenced]) -> String {
    let mut s = String::from(
        "# Variables bedouin.yaml reads. Loaded before facts resolve, so what\n\
         # you set here reaches `{{ env.NAME }}` and `match: { env: ... }`.\n\
         # Your shell wins over this file, and this file belongs in .gitignore.\n\
         #\n\
         # Not the place for real secrets: prefer your shell, or a secret\n\
         # manager your rc files already read.\n\n",
    );
    for r in refs {
        s.push_str(&format!("# {} -- {}\n", r.name, r.site));
        if r.set {
            // Already in the environment: leave it commented so the file does
            // not silently shadow a value that is working.
            s.push_str(&format!("# {}=\n\n", r.name));
        } else {
            s.push_str(&format!("{}=\n\n", r.name));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(y: &str) -> RawConfig {
        serde_yaml_ng::from_str(y).expect("parses")
    }

    #[test]
    fn a_dotenv_file_is_parsed_without_shell_semantics() {
        let m = parse(
            "# a comment\n\nA=1\nexport B=two\nC=\"quoted value\"\nD='single'\n\
             NOEQUALS\n=novalue\nE=has=equals\n",
        );
        assert_eq!(m["A"], "1");
        assert_eq!(m["B"], "two", "`export ` prefix is tolerated");
        assert_eq!(m["C"], "quoted value");
        assert_eq!(m["D"], "single");
        assert_eq!(m["E"], "has=equals", "only the first = splits");
        assert!(!m.contains_key("NOEQUALS"));
        assert!(!m.contains_key(""));
    }

    #[test]
    fn every_read_site_is_reported_with_whether_it_can_fail() {
        let r = raw(r#"
version: 0
vars:
  v: "{{ env.PLAIN }}"
targets:
  - name: work
    match: { env: { BEDOUIN_PROFILE: work } }
packages:
  - name: zellij
    from: cargo
    version: "{{ env.GUARDED | default('latest') }}"
"#);
        let env = BTreeMap::from([("BEDOUIN_PROFILE".to_string(), "work".to_string())]);
        let refs = referenced(&r, &env);
        let by = |n: &str| refs.iter().find(|x| x.name == n).unwrap().clone();

        assert!(by("BEDOUIN_PROFILE").set);
        assert_eq!(by("BEDOUIN_PROFILE").site, "targets.work");

        // A bare reference is a resolve-time failure waiting to happen.
        assert!(!by("PLAIN").set);
        assert!(!by("PLAIN").has_default);
        assert_eq!(by("PLAIN").site, "vars.v");

        // A guarded one cannot fail, so it is not worth warning about.
        assert!(by("GUARDED").has_default);
        assert_eq!(by("GUARDED").site, "packages.zellij");
    }

    #[test]
    fn an_empty_value_does_not_count_as_set() {
        let r = raw("version: 0\nvars:\n  v: \"{{ env.E }}\"\npackages: [{name: jq, from: apt}]\n");
        let env = BTreeMap::from([("E".to_string(), String::new())]);
        assert!(
            !referenced(&r, &env)[0].set,
            "the scaffold's own blank line is not a value"
        );
    }

    #[test]
    fn the_scaffold_leaves_already_set_variables_commented() {
        // Uncommenting a value that is already working would let the file
        // silently shadow it with an empty string.
        let refs = vec![
            Referenced {
                name: "SET".into(),
                site: "vars.a".into(),
                set: true,
                has_default: false,
            },
            Referenced {
                name: "UNSET".into(),
                site: "vars.b".into(),
                set: false,
                has_default: false,
            },
        ];
        let s = scaffold(&refs);
        assert!(s.contains("# SET=\n"), "{s}");
        assert!(s.contains("\nUNSET=\n"), "{s}");
        assert!(s.contains(".gitignore"), "says where it belongs: {s}");
    }

    #[test]
    fn no_values_are_ever_printed() {
        // Same rule as `bedouin facts`: this output lands in bug reports.
        let r =
            raw("version: 0\nvars:\n  v: \"{{ env.TOKEN }}\"\npackages: [{name: jq, from: apt}]\n");
        let env = BTreeMap::from([("TOKEN".to_string(), "hunter2".to_string())]);
        let rendered = format!("{:?}", referenced(&r, &env));
        assert!(!rendered.contains("hunter2"));
        assert!(!scaffold(&referenced(&r, &env)).contains("hunter2"));
    }
}
