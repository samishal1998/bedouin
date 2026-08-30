//! The minijinja pass, and the template context.
//!
//! Rendering runs *after* arm selection, so losing arms are never evaluated: a
//! `{{ home }}/.cargo/bin` inside a `ubuntu:` arm costs nothing on macOS, and a
//! template that is only valid on one platform is free on the others.
//!
//! The environment runs with [`UndefinedBehavior::Strict`]. The default is
//! lenient, under which `{{ hom }}/.cargo/bin` renders as `/.cargo/bin` and
//! ships a wrong PATH entry in silence -- which would contradict the whole
//! argument for a closed arm vocabulary. Strict still lets the `default` filter
//! absorb an undefined, so `{{ env.X | default('latest') }}` keeps working.

use crate::facts::Facts;
use crate::value::Tmpl;
use minijinja::{Environment, UndefinedBehavior};
use std::collections::BTreeMap;

/// Everything a template may name. Facts are bare, user variables live under
/// `vars.`, and the environment under `env.`. Nothing else is in scope.
pub struct Context<'a> {
    pub facts: &'a Facts,
    pub vars: &'a BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderError {
    pub template: String,
    pub message: String,
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rendering `{}`: {}", self.template, self.message)
    }
}

fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    // minijinja drops a template's final newline by default. A managed file is
    // a file, and POSIX text files end with one -- without this every rendered
    // dotfile silently loses its last line ending.
    env.set_keep_trailing_newline(true);
    env
}

impl Context<'_> {
    fn bind(&self) -> minijinja::Value {
        let f = self.facts;
        minijinja::context! {
            os => f.os.as_str(),
            arch => f.arch.as_str(),
            distro => f.distro.as_str(),
            distro_like => f.distro_like.as_str(),
            distro_version => f.distro_version.as_str(),
            home => f.home.to_string_lossy(),
            user => f.user.as_str(),
            hostname => f.hostname.as_str(),
            shell => minijinja::context! {
                name => f.shell.name.as_str(),
                detected => f.shell.detected.as_str(),
                rc_file => f.shell.rc_file.to_string_lossy(),
                rc_dir => f.shell.rc_dir.to_string_lossy(),
            },
            vars => self.vars,
            env => &f.env,
        }
    }
}

/// Render one template. Literals skip minijinja entirely.
pub fn render(t: &Tmpl, ctx: &Context<'_>) -> Result<String, RenderError> {
    if t.is_literal() {
        return Ok(t.0.clone());
    }
    environment()
        .render_str(&t.0, ctx.bind())
        .map_err(|e| RenderError {
            template: t.0.clone(),
            // minijinja's Display carries the useful part; the Debug form
            // repeats the template back, which we already have.
            message: e.to_string(),
        })
}

/// Variables cannot reference other variables, which is what keeps resolution
/// two flat layers rather than a fixpoint. So they render without `vars` in
/// scope, and this is the context that does it.
pub fn render_var(t: &Tmpl, facts: &Facts) -> Result<String, RenderError> {
    let empty = BTreeMap::new();
    render(
        t,
        &Context {
            facts,
            vars: &empty,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Arch, Distro, Os};

    fn ctx_facts() -> Facts {
        let mut f = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
        f.env.insert("PRESENT".into(), "yes".into());
        f
    }

    #[test]
    fn facts_are_bare_and_vars_and_env_are_namespaced() {
        let f = ctx_facts();
        let vars = BTreeMap::from([("editor".to_string(), "nvim".to_string())]);
        let c = Context {
            facts: &f,
            vars: &vars,
        };
        assert_eq!(render(&"{{ os }}".into(), &c).unwrap(), "linux");
        assert_eq!(
            render(&"{{ home }}/.cargo/bin".into(), &c).unwrap(),
            "/home/tester/.cargo/bin"
        );
        assert_eq!(render(&"{{ vars.editor }}".into(), &c).unwrap(), "nvim");
        assert_eq!(render(&"{{ env.PRESENT }}".into(), &c).unwrap(), "yes");
        assert_eq!(
            render(&"{{ shell.rc_dir }}/70-zellij.zsh".into(), &c).unwrap(),
            "/home/tester/.zshrc.d/70-zellij.zsh"
        );
    }

    #[test]
    fn a_typo_in_a_name_is_an_error_not_an_empty_string() {
        // Under minijinja's default leniency this renders "/.cargo/bin" and
        // ships a wrong PATH entry without a word.
        let f = ctx_facts();
        let vars = BTreeMap::new();
        let c = Context {
            facts: &f,
            vars: &vars,
        };
        let err = render(&"{{ hom }}/.cargo/bin".into(), &c).unwrap_err();
        assert!(err.message.contains("undefined"), "{err}");
        assert!(render(&"{{ vars.nope }}".into(), &c).is_err());
    }

    #[test]
    fn the_default_filter_still_absorbs_an_undefined_under_strict() {
        // §6.5 rejects `fromEnv` on the grounds that this idiom replaces it, so
        // strictness must not break it.
        let f = ctx_facts();
        let vars = BTreeMap::new();
        let c = Context {
            facts: &f,
            vars: &vars,
        };
        assert_eq!(
            render(&"{{ env.MISSING | default('latest') }}".into(), &c).unwrap(),
            "latest"
        );
        assert_eq!(
            render(&"{{ env.PRESENT | default('latest') }}".into(), &c).unwrap(),
            "yes"
        );
    }

    #[test]
    fn a_rendered_file_keeps_its_trailing_newline() {
        let f = ctx_facts();
        let vars = BTreeMap::from([("editor".to_string(), "nvim".to_string())]);
        let c = Context {
            facts: &f,
            vars: &vars,
        };
        assert_eq!(
            render(&"[core]\n\teditor = {{ vars.editor }}\n".into(), &c).unwrap(),
            "[core]\n\teditor = nvim\n"
        );
    }

    #[test]
    fn a_literal_is_returned_untouched() {
        let f = ctx_facts();
        let vars = BTreeMap::new();
        let c = Context {
            facts: &f,
            vars: &vars,
        };
        // No template syntax, so no engine, and braces in shell code survive.
        assert_eq!(render(&"latest".into(), &c).unwrap(), "latest");
        assert_eq!(
            render(&"eval \"$(zellij setup)\"".into(), &c).unwrap(),
            "eval \"$(zellij setup)\""
        );
    }

    #[test]
    fn a_variable_cannot_reference_another_variable() {
        let f = ctx_facts();
        assert!(render_var(&"{{ vars.other }}".into(), &f).is_err());
        assert_eq!(render_var(&"{{ os }}-box".into(), &f).unwrap(), "linux-box");
    }
}
