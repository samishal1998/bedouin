#!/bin/sh
# Layer 3 for what 0.16.0 added: `subdir:` export and lifecycle hooks. Neither
# had container coverage on any distro. The export shells out to
# `git archive | tar -x --strip-components`, which is two more binaries than
# the rest of bedouin needs -- and the ssh bootstrap installs neither.
set -eu

echo "== the tools the export depends on =="
command -v git >/dev/null || { echo "FAIL: no git"; exit 1; }
command -v tar >/dev/null || { echo "FAIL: no tar -- subdir export cannot work here"; exit 1; }
echo "OK: git $(git --version | awk '{print $3}'), tar present"

echo "== an upstream with a subdir =="
rm -rf /tmp/work /tmp/upstream.git
mkdir -p /tmp/work/nvim/lua
cd /tmp/work
git init -q -b main .
git config user.email smoke@bedouin.test
git config user.name smoke
printf 'vim.o.number = true\n' > nvim/init.lua
printf 'return {}\n' > nvim/lua/plugins.lua
# Outside the subdir: must NOT arrive at dest.
printf 'not yours\n' > README.md
git add -A && git commit -qm first
git clone -q --bare /tmp/work /tmp/upstream.git
git remote add origin /tmp/upstream.git

echo "== plan =="
rm -f /tmp/hooks.log
/bedouin --config /cfg/bedouin.yaml plan || [ $? -eq 2 ]

echo "== apply =="
/bedouin --config /cfg/bedouin.yaml apply -y

echo "== the subdir landed at the root of dest =="
[ -f "$HOME/.config/nvim/init.lua" ] \
  || { echo "FAIL: init.lua is not at the root of dest"; exit 1; }
[ -f "$HOME/.config/nvim/lua/plugins.lua" ] \
  || { echo "FAIL: nested file did not survive the export"; exit 1; }
[ ! -e "$HOME/.config/nvim/nvim" ] \
  || { echo "FAIL: strip-components left the subdir level in place"; exit 1; }
[ ! -e "$HOME/.config/nvim/README.md" ] \
  || { echo "FAIL: a file outside the subdir was exported"; exit 1; }
[ ! -e "$HOME/.config/nvim/.git" ] \
  || { echo "FAIL: dest is a clone, not a snapshot"; exit 1; }
echo "OK: dest holds the subdir's contents and nothing else"

echo "== the hooks fired, in order, with their step env =="
grep -q '^before_apply$' /tmp/hooks.log || { echo "FAIL: before_apply did not run"; exit 1; }
grep -q '^before_step repo/.* create$' /tmp/hooks.log || { echo "FAIL: before_step lacks step/action env"; exit 1; }
grep -q '^after_step repo/.* create$' /tmp/hooks.log || { echo "FAIL: after_step lacks step/action env"; exit 1; }
grep -q '^after_apply ok$' /tmp/hooks.log || { echo "FAIL: after_apply lacks BEDOUIN_STATUS=ok"; exit 1; }
[ "$(head -1 /tmp/hooks.log)" = before_apply ] \
  || { echo "FAIL: before_apply was not first"; exit 1; }
[ "$(tail -1 /tmp/hooks.log | cut -d' ' -f1)" = after_apply ] \
  || { echo "FAIL: after_apply was not last"; exit 1; }
echo "OK: $(wc -l < /tmp/hooks.log) hook lines, before_apply first, after_apply last"

echo "== converges =="
/bedouin --config /cfg/bedouin.yaml plan >/dev/null && echo "OK: second plan exits 0"

echo "== the snapshot follows the branch, on sync =="
# Deliberately NOT on apply: plan decides offline, so a remote that moved is
# not knowable there without breaking convergence. `sync` is the command whose
# job is "go and get what changed" -- and it pulls the config repo first, so
# the config has to live in a real clone with a tracking branch.
mkdir -p /tmp/cfgsrc && cp /cfg/bedouin.yaml /tmp/cfgsrc/
cd /tmp/cfgsrc
git init -q -b main .
git config user.email smoke@bedouin.test
git config user.name smoke
git add -A && git commit -qm cfg
git clone -q --bare /tmp/cfgsrc /tmp/cfgremote.git
rm -rf /tmp/cfg && git clone -q /tmp/cfgremote.git /tmp/cfg

cd /tmp/work
printf 'return { "new" }\n' > nvim/lua/added.lua
git rm -q nvim/lua/plugins.lua
git add -A && git commit -qm second
git push -q origin main

/bedouin --config /tmp/cfg/bedouin.yaml sync -y
[ -f "$HOME/.config/nvim/lua/added.lua" ] \
  || { echo "FAIL: a new upstream file did not arrive on sync"; exit 1; }
[ ! -e "$HOME/.config/nvim/lua/plugins.lua" ] \
  || { echo "FAIL: a deleted upstream file survived -- dest is not a snapshot"; exit 1; }
echo "OK: sync tracks the branch in both directions"

echo "== apply does not chase the remote =="
# The other half of that decision: having synced, a plain apply is a no-op and
# does not re-export. If this ever starts reporting changes, convergence broke.
/bedouin --config /cfg/bedouin.yaml plan >/dev/null \
  || { echo "FAIL: apply wants to re-export a repo that has not changed in the config"; exit 1; }
echo "OK: plan still exits 0 -- a moved remote is sync's business, not plan's"

echo "== a subdir that does not exist refuses without emptying dest =="
# The review's finding: the existence check has to run BEFORE dest is cleared,
# or a typo costs you the directory.
sed 's|subdir: nvim|subdir: nvimm|' /cfg/bedouin.yaml > /tmp/typo.yaml
if /bedouin --config /tmp/typo.yaml apply -y >/tmp/typo.log 2>&1; then
  echo "FAIL: a missing subdir applied cleanly"; exit 1
fi
grep -q 'check subdir' /tmp/typo.log || { echo "FAIL: no sentence about the subdir:"; sed -n '1,20p' /tmp/typo.log; exit 1; }
[ -f "$HOME/.config/nvim/init.lua" ] \
  || { echo "FAIL: the refusal emptied dest anyway"; exit 1; }
echo "OK: refused, and dest is intact"

echo "== a failing hook stops the run, and on_failure observes it =="
cat > /tmp/hookfail.yaml <<'YAML'
version: 0
shell: bash
hooks:
  before_apply: echo "nope" >&2; exit 1
  on_failure: echo "on_failure $BEDOUIN_STEP" >> /tmp/hooks2.log
repos:
  - url: file:///tmp/upstream.git
    dest: "{{ home }}/.config/other"
    ref: main
    subdir: nvim
YAML
rm -f /tmp/hooks2.log
if /bedouin --config /tmp/hookfail.yaml apply -y >/tmp/hookfail.log 2>&1; then
  echo "FAIL: a failing before_apply did not stop the run"; exit 1
fi
[ ! -e "$HOME/.config/other" ] \
  || { echo "FAIL: before_apply failed but the step ran anyway"; exit 1; }
grep -q 'before_apply' /tmp/hookfail.log \
  || { echo "FAIL: the failure does not name the hook"; sed -n '1,20p' /tmp/hookfail.log; exit 1; }
echo "OK: before_apply failing stopped the run before the first step"

echo "== no tar is a sentence, and it does not cost you dest =="
# openSUSE Leap's base image has no tar and git does not pull one in, so this
# is a real configuration rather than a hypothetical. Tested through `sync`,
# which re-exports unconditionally: a plain apply would find the repo already
# done, skip the step, and prove nothing. The check has to fire before dest is
# cleared, so a machine with no tar keeps the snapshot it already had.
TARBIN=$(command -v tar)
mv "$TARBIN" "$TARBIN.hidden"
set +e
/bedouin --config /tmp/cfg/bedouin.yaml sync -y >/tmp/notar.log 2>&1
rc=$?
set -e
mv "$TARBIN.hidden" "$TARBIN"
[ "$rc" -ne 0 ] || { echo "FAIL: sync reported success with no tar"; exit 1; }
grep -q 'exports with tar' /tmp/notar.log \
  || { echo "FAIL: no sentence naming tar"; sed -n '1,20p' /tmp/notar.log; exit 1; }
[ -f "$HOME/.config/nvim/init.lua" ] \
  || { echo "FAIL: the refusal emptied dest anyway"; exit 1; }
echo "OK: refused by name, and the snapshot survived"
