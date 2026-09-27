#!/bin/sh
# git sync against a local bare "remote": no network, and nothing is applied
# (edits use --no-apply, sync is answered "n"). Usage: tests/gitsync.sh BEDOUIN
set -eu
B=$(realpath "$1"); T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
git init -q --bare "$T/remote.git"
git clone -q "$T/remote.git" "$T/cfg" 2>/dev/null
cd "$T/cfg"
git config user.email t@t; git config user.name t
printf 'version: 0\nshell: bash\npackages:\n  - {name: git, from: apt}\n' > bedouin.yaml
git add -A; git commit -qm init; git push -q -u origin HEAD 2>/dev/null
remote() { git --git-dir="$T/remote.git" log --oneline | wc -l; }

echo "== on by default: an edit is committed and pushed"
"$B" --config bedouin.yaml add apt:tree --no-apply | grep -q "committed and pushed" || { echo FAIL; exit 1; }
[ "$(remote)" = 2 ] && [ -z "$(git status --porcelain)" ] || { echo "FAIL: not pushed"; exit 1; }

echo "== off: edits stay local"
"$B" --config bedouin.yaml sync --auto off >/dev/null
"$B" --config bedouin.yaml add apt:jq --no-apply >/dev/null
"$B" --config bedouin.yaml add apt:htop --no-apply >/dev/null
[ "$(remote)" = 2 ] && [ -n "$(git status --porcelain)" ] || { echo "FAIL: synced while off"; exit 1; }

echo "== sync commits the batch in one commit and pushes it"
echo n | "$B" --config bedouin.yaml sync >/dev/null
[ "$(remote)" = 3 ] && [ -z "$(git status --porcelain)" ] || { echo "FAIL: batch not pushed"; exit 1; }

echo "== sync replays local commits on top of a moved remote"
git clone -q "$T/remote.git" "$T/other" && (cd "$T/other" && git config user.email t@t && git config user.name t \
  && echo "# from elsewhere" >> bedouin.yaml && git commit -qam elsewhere && git push -q)
git commit -q --allow-empty -m local
echo n | "$B" --config bedouin.yaml sync >/dev/null
[ "$(remote)" = 5 ] && grep -q "from elsewhere" bedouin.yaml || { echo "FAIL: rebase-pull"; exit 1; }
echo OK
