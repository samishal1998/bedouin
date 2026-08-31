use bedouin_core::artifact::scan_refs;
use bedouin_core::facts::{Arch, Distro, Facts, Os};
use bedouin_core::render::{render, Context};
use std::collections::BTreeMap;

#[test]
fn guard_verdicts_match_real_render_behaviour() {
    let mut f = Facts::fixture(Os::Linux, Distro::Ubuntu, Arch::X86_64);
    f.env.clear();
    let vars = BTreeMap::new();
    let c = Context {
        facts: &f,
        vars: &vars,
    };
    let cases = [
        "{{ env.MISSING }}",
        "{{ env.MISSING | default('z') }}",
        "{{ env.MISSING | d('z') }}",
        "{{ env.MISSING | default }}",
        "{{ env.MISSING | default(8080) }}",
        "{{ env.A | default('}}') }}",
        "{{ env['SUB'] }}",
        "{{ env['SUB'] | default('z') }}",
        "{% if env.OPT is defined %}x{% endif %}",
        "{% if env.OPT is not defined %}x{% endif %}",
        "{{ env.CONFIG_DIR ~ '/defaults.toml' }}",
        "{% if env.PROFILE == 'default' %}a{% endif %}",
        "{{ env.E | default(vars.other) }}",
        "{% raw %}{{ env.LITERAL }}{% endraw %}",
        "{# {{ env.LEGACY }} #}",
        "{{ env.A }}{{ env.B | default('x') }}",
    ];
    let mut bad = vec![];
    for t in cases {
        let refs = scan_refs(t);
        // A template every one of whose reads we call guarded must render.
        let all_guarded = !refs.is_empty() && refs.iter().all(|r| r.guarded);
        let renders = render(&(*t).into(), &c).is_ok();
        if all_guarded && !renders {
            bad.push(format!("SAID SAFE BUT FAILS: {t}  {refs:?}"));
        }
        // Claiming no reads at all must also mean it renders.
        if refs.is_empty() && !renders {
            bad.push(format!("SAID NO READS BUT FAILS: {t}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
