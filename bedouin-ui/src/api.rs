//! The read model, as JSON.
//!
//! One endpoint rather than nine. The page is a single view over one plan, and
//! nine round-trips would each re-probe the machine — `run::plan` is the
//! expensive call, and it already produces everything the browser needs.

use bedouin_core::host::OsHost;
use bedouin_core::plan::{action_label, kind_label};
use bedouin_core::{doctor, envfile, run};
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
pub struct Snapshot {
    pub version: &'static str,
    pub config_path: String,
    pub machine: Machine,
    pub plan: PlanView,
    pub config: ConfigView,
    pub doctor: Vec<DriftView>,
    pub env: Vec<EnvView>,
}

#[derive(Serialize)]
pub struct Machine {
    pub os: String,
    pub distro: String,
    pub distro_like: String,
    pub arch: String,
    pub hostname: String,
    pub user: String,
    pub shell: String,
    pub privilege: String,
    pub managers: Vec<String>,
}

#[derive(Serialize)]
pub struct PlanView {
    pub items: Vec<ItemView>,
    pub warnings: Vec<String>,
    pub pruned: Vec<String>,
    pub counts: Counts,
}

#[derive(Serialize)]
pub struct Counts {
    pub add: usize,
    pub change: usize,
    pub remove: usize,
}

/// Flattened for the browser: `sigil` and `kind` are what the list draws, and
/// deriving them here keeps one answer rather than one per client.
#[derive(Serialize)]
pub struct ItemView {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub detail: String,
    pub sigil: String,
    pub action: String,
    pub needs_root: bool,
    pub arms: Vec<[String; 2]>,
    pub fields: Vec<[String; 2]>,
}

#[derive(Serialize, Default)]
pub struct ConfigView {
    pub packages: Vec<ItemView>,
    pub files: Vec<ItemView>,
    pub repos: Vec<ItemView>,
    pub links: Vec<ItemView>,
    pub aliases: Vec<ItemView>,
    pub languages: Vec<ItemView>,
}

#[derive(Serialize)]
pub struct DriftView {
    pub id: String,
    pub what: String,
    pub detail: String,
}

#[derive(Serialize)]
pub struct EnvView {
    pub name: String,
    pub site: String,
    pub set: bool,
    pub has_default: bool,
    pub match_key: bool,
}

pub fn snapshot(config: Option<&Path>, cwd: &Path) -> Result<Snapshot, String> {
    let host = OsHost::new();
    let o = run::plan(&host, config, cwd).map_err(|e| e.to_string())?;

    let (add, change, remove) = o.plan.counts();
    let items: Vec<ItemView> = o
        .plan
        .changes()
        .map(|i| ItemView {
            id: i.id.clone(),
            kind: kind_label(i.kind).to_string(),
            name: i.name.clone(),
            detail: i.detail.clone(),
            sigil: i.action.sigil().to_string(),
            action: action_label(&i.action),
            needs_root: i.needs_root,
            arms: i.arms.iter().map(|(k, v)| [k.clone(), v.clone()]).collect(),
            fields: vec![],
        })
        .collect();

    let f = &o.facts;
    let machine = Machine {
        os: f.os.to_string(),
        distro: f.distro.to_string(),
        distro_like: f.distro_like.to_string(),
        arch: f.arch.to_string(),
        hostname: f.hostname.clone(),
        user: f.user.clone(),
        shell: f.shell.name.to_string(),
        privilege: f.privilege.to_string(),
        managers: f.managers.iter().map(|m| m.to_string()).collect(),
    };

    let sigil_for = |id: &str| {
        o.plan
            .items
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.action.sigil().to_string())
            .unwrap_or_else(|| " ".into())
    };

    let mut config_view = ConfigView::default();
    for p in &o.config.packages {
        let mut fields = vec![[
            "from".to_string(),
            if p.from.is_empty() {
                "script".into()
            } else {
                p.from
                    .iter()
                    .map(|m| m.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        ]];
        fields.push([
            "version".into(),
            p.version.clone().unwrap_or_else(|| "latest".into()),
        ]);
        if !p.needs.is_empty() {
            fields.push(["needs".into(), p.needs.join(", ")]);
        }
        if !p.path.is_empty() {
            fields.push(["path".into(), p.path.join(", ")]);
        }
        for (k, v) in &p.aliases {
            fields.push([format!("alias {k}"), v.clone()]);
        }
        if let Some(c) = &p.completions {
            fields.push(["completions".into(), c.join(" ")]);
        }
        for b in &p.rc {
            fields.push([format!("rc {}", b.file), b.content.trim().to_string()]);
        }
        if let Some(sc) = &p.script {
            fields.push(["script".into(), sc.trim().to_string()]);
        }
        config_view.packages.push(ItemView {
            id: format!("package/{}", p.name),
            kind: p
                .from
                .first()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "script".into()),
            name: p.name.clone(),
            detail: p.version.clone().unwrap_or_else(|| "latest".into()),
            sigil: sigil_for(&format!("package/{}", p.name)),
            action: String::new(),
            needs_root: false,
            arms: vec![],
            fields,
        });
    }
    for x in &o.config.files {
        config_view.files.push(ItemView {
            id: format!("file/{}", x.dest),
            kind: "file".into(),
            name: x.dest.clone(),
            detail: x.src.clone(),
            sigil: " ".into(),
            action: String::new(),
            needs_root: false,
            arms: vec![],
            fields: vec![
                ["src".into(), x.src.clone()],
                ["dest".into(), x.dest.clone()],
                [
                    "mode".into(),
                    x.mode.clone().unwrap_or_else(|| "0644".into()),
                ],
            ],
        });
    }
    for x in &o.config.repos {
        config_view.repos.push(ItemView {
            id: format!("repo/{}", x.dest),
            kind: "repo".into(),
            name: x.dest.clone(),
            detail: x.url.clone(),
            sigil: " ".into(),
            action: String::new(),
            needs_root: false,
            arms: vec![],
            fields: vec![
                ["url".into(), x.url.clone()],
                ["dest".into(), x.dest.clone()],
            ],
        });
    }
    for x in &o.config.links {
        config_view.links.push(ItemView {
            id: format!("link/{}", x.dest),
            kind: "link".into(),
            name: x.dest.clone(),
            detail: format!("-> {}", x.src),
            sigil: " ".into(),
            action: String::new(),
            needs_root: false,
            arms: vec![],
            fields: vec![
                ["points at".into(), x.src.clone()],
                ["link".into(), x.dest.clone()],
            ],
        });
    }
    for (k, v) in &o.config.aliases {
        config_view.aliases.push(ItemView {
            id: format!("alias/{k}"),
            kind: "alias".into(),
            name: k.clone(),
            detail: v.clone(),
            sigil: " ".into(),
            action: String::new(),
            needs_root: false,
            arms: vec![],
            fields: vec![["expands to".into(), v.clone()]],
        });
    }
    for l in &o.config.languages {
        config_view.languages.push(ItemView {
            id: format!("language/{}", l.name),
            kind: l
                .installer
                .map(|m| m.to_string())
                .unwrap_or_else(|| "default".into()),
            name: l.name.clone(),
            detail: l.version.clone().unwrap_or_else(|| "latest".into()),
            sigil: sigil_for(&format!("language/{}", l.name)),
            action: String::new(),
            needs_root: false,
            arms: vec![],
            fields: vec![
                [
                    "installer".into(),
                    l.installer
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| "its own (default)".into()),
                ],
                [
                    "version".into(),
                    l.version.clone().unwrap_or_else(|| "latest".into()),
                ],
            ],
        });
    }

    let drift = doctor::check(&o.state, &o.config, &o.facts, &host)
        .map(|r| {
            r.drift
                .iter()
                .map(|d| DriftView {
                    id: d.id().to_string(),
                    what: drift_kind(d).into(),
                    detail: d.to_string().trim().replace('\n', " "),
                })
                .collect()
        })
        .unwrap_or_default();

    let env = envfile::referenced(&o.loaded.raw, &o.facts.env, &o.loaded.root, &host)
        .into_iter()
        .map(|r| EnvView {
            name: r.name,
            site: r.site,
            set: r.set,
            has_default: r.has_default,
            match_key: r.match_key,
        })
        .collect();

    Ok(Snapshot {
        version: env!("CARGO_PKG_VERSION"),
        config_path: o.loaded.entry.display().to_string(),
        machine,
        plan: PlanView {
            items,
            warnings: o.plan.warnings.clone(),
            pruned: o.plan.pruned.clone(),
            counts: Counts {
                add,
                change,
                remove,
            },
        },
        config: config_view,
        doctor: drift,
        env,
    })
}

fn drift_kind(d: &doctor::Drift) -> &'static str {
    use doctor::Drift::*;
    match d {
        Edited { .. } => "edited by hand",
        Missing { .. } => "gone",
        Resolved { .. } => "resolves differently",
        Unterminated { .. } => "block never closed",
        Incomplete { .. } => "never finished",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// A config on disk, because `snapshot` drives the real host: that is the
    /// point of it, and a fake here would be testing the fake.
    fn fixture() -> PathBuf {
        let dir = std::env::temp_dir().join("bedouin-ui-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("templates")).unwrap();
        fs::write(
            dir.join("bedouin.yaml"),
            "version: 0\nshell: bash\naliases:\n  ll: ls -alh\npackages:\n  \
             - name: jq\n    from: apt\nfiles:\n  \
             - { src: templates/g.j2, dest: \"~/.gitconfig\" }\n",
        )
        .unwrap();
        fs::write(dir.join("templates/g.j2"), "[user]\n").unwrap();
        dir
    }

    #[test]
    fn the_snapshot_carries_every_key_the_page_reads() {
        let dir = fixture();
        let s = snapshot(Some(&dir.join("bedouin.yaml")), &dir).expect("snapshot");
        let v = serde_json::to_value(&s).expect("serialises");

        // The page indexes these by name. A rename here is a blank screen
        // there, and nothing else would catch it.
        for key in [
            "version",
            "config_path",
            "machine",
            "plan",
            "config",
            "doctor",
            "env",
        ] {
            assert!(v.get(key).is_some(), "snapshot lost `{key}`");
        }
        for key in [
            "packages",
            "files",
            "repos",
            "links",
            "aliases",
            "languages",
        ] {
            assert!(v["config"].get(key).is_some(), "config lost `{key}`");
        }
        for key in ["items", "warnings", "pruned", "counts"] {
            assert!(v["plan"].get(key).is_some(), "plan lost `{key}`");
        }
        for key in ["add", "change", "remove"] {
            assert!(
                v["plan"]["counts"].get(key).is_some(),
                "counts lost `{key}`"
            );
        }

        let row = &v["config"]["packages"][0];
        for key in ["id", "kind", "name", "detail", "sigil", "fields"] {
            assert!(row.get(key).is_some(), "a row lost `{key}`: {row}");
        }
        assert_eq!(row["name"], "jq");
        assert_eq!(v["config"]["aliases"][0]["name"], "ll");
    }

    #[test]
    fn the_page_is_embedded_not_looked_for_on_disk() {
        // The sidecar is fetched into a directory of its own, so anything it
        // expected to find beside itself would not be there.
        let page = include_str!("../web/dist/index.html");
        assert!(page.contains("<title>bedouin</title>"), "no page embedded");
        assert!(
            !page.contains("was not built into this binary"),
            "this binary carries the build.rs placeholder, not the real page \
             -- run `npm ci && npm run build` in bedouin-ui/web"
        );
    }
}
