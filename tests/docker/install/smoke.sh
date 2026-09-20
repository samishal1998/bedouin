#!/bin/sh
# Layer 3 for `bedouin install` and `from: github`. Everything here needs the
# network, which is the reason it is its own fixture: the other smoke tests
# work on a machine with none.
set -eu
export PATH="$HOME/.local/bin:$PATH"

echo "== the one-off verb =="
# ripgrep's binary is `rg`, not `ripgrep`. Installing it under the repository
# name would be a working binary under a name nobody types.
/bedouin --config /cfg/bedouin.yaml install BurntSushi/ripgrep@15.2.0 -y
command -v rg >/dev/null || { echo "FAIL: installed as something other than rg"; ls "$HOME/.local/bin"; exit 1; }
rg --version | head -1
echo "OK: the archive named the binary"

echo "== it refuses rather than guessing =="
# helix ships a source tarball beside its binaries, and .tar.xz needs a
# program this image does not have. Either way the answer is a sentence.
if /bedouin --config /cfg/bedouin.yaml install helix-editor/helix -y >/tmp/hx.log 2>&1; then
  grep -q "helix" /tmp/hx.log || true
  echo "note: helix installed; this image has xz"
else
  grep -qE "needs .xz.|fit this machine equally well|runs on" /tmp/hx.log \
    || { echo "FAIL: refused for an unexpected reason:"; cat /tmp/hx.log; exit 1; }
  echo "OK: refused with a reason"
fi
# Whatever happened, it must never have taken the source archive.
[ ! -e "$HOME/.local/bin/helix-25.07.1-source.tar.xz" ] || { echo "FAIL: installed source"; exit 1; }

echo "== declared, not just installed =="
/bedouin --config /cfg/bedouin.yaml plan || [ $? -eq 2 ]
/bedouin --config /cfg/bedouin.yaml apply -y
command -v lazygit >/dev/null || { echo "FAIL: lazygit is not on PATH"; exit 1; }
lazygit --version | head -1

echo "== converges =="
/bedouin --config /cfg/bedouin.yaml plan >/dev/null && echo "OK: second plan exits 0"

echo "== the pinned one is recorded at its tag =="
grep -A5 '"package/BurntSushi/ripgrep"' "$HOME/.local/state/bedouin/state.json" \
  | grep -q '"version": "15.2.0"' \
  || { echo "FAIL: the tag was not recorded"; exit 1; }
echo "OK: state records the tag"

echo "== a rename must not delete the file it just wrote =="
# The 0.20.1 case: two package names reduce to one derived filename, so the
# same path is both added (by the new item) and removed (with the old one).
mkdir -p /tmp/c && cp /cfg/bedouin.yaml /tmp/c/
cat >> /tmp/c/bedouin.yaml <<'YAML'
  - name: sharkdp/fd
    from: github
    aliases: { f: fd }
YAML
/bedouin --config /tmp/c/bedouin.yaml apply -y >/dev/null
ALIAS="$HOME/.bashrc.d/30-fd-aliases.bash"
[ -f "$ALIAS" ] || { echo "FAIL: alias file was not written"; exit 1; }
# Rename the package; the alias file it derives is identical.
sed -i 's|- name: sharkdp/fd|- name: sharkdp-mirror/fd|' /tmp/c/bedouin.yaml
/bedouin --config /tmp/c/bedouin.yaml plan >/dev/null 2>&1 || true
/bedouin --config /tmp/c/bedouin.yaml apply -y >/dev/null 2>&1 || true
[ -f "$ALIAS" ] || { echo "FAIL: the rename deleted the file it had just written"; exit 1; }
grep -q "alias f=" "$ALIAS" || { echo "FAIL: the file survived but is empty"; exit 1; }
echo "OK: the surviving owner kept the file"

echo "== no slash ever became a directory =="
# Every filename bedouin derives from `org/repo` must be one path component.
! find "$HOME/.bashrc.d" -type d -name '30-*' | grep -q . \
  || { echo "FAIL: a package name created a directory"; find "$HOME/.bashrc.d" -type d; exit 1; }
echo "OK"
