#!/bin/sh
# Layer 3 for the distros bedouin could not manage packages on. Arch had a
# Distro variant and no Manager; Alpine had neither, which is why the ssh
# bootstrap refused it by name.
set -eu

echo "== the machine identifies itself =="
/bedouin --config /cfg/bedouin.yaml facts | grep -E '"(distro|distro_like)"'
MGR=$(/bedouin --config /cfg/bedouin.yaml facts | sed -n '/"managers"/,/]/p' | tr -d ' \n"')
echo "  managers: $MGR"

echo "== apply =="
/bedouin --config /cfg/bedouin.yaml plan || [ $? -eq 2 ]
/bedouin --config /cfg/bedouin.yaml apply -y

echo "== the package is really installed =="
command -v jq >/dev/null || { echo "FAIL: jq is not on PATH"; exit 1; }
jq --version
echo "== converges =="
/bedouin --config /cfg/bedouin.yaml plan >/dev/null && echo "OK: second plan exits 0"

echo "== a hand-installed package is found by pickup and adopted, not claimed =="
# tree is not declared anywhere; installing it by hand is the whole scenario.
if command -v pacman >/dev/null 2>&1; then pacman -S --noconfirm --needed tree >/dev/null 2>&1
elif command -v apk >/dev/null 2>&1; then apk add --no-cache tree >/dev/null 2>&1
elif command -v apt-get >/dev/null 2>&1; then apt-get install -y -qq tree >/dev/null 2>&1
fi
/bedouin --config /cfg/bedouin.yaml pickup | grep -q tree \
  || { echo "FAIL: pickup did not notice a hand-installed package"; \
       /bedouin --config /cfg/bedouin.yaml pickup; exit 1; }
echo "OK: pickup found it"

# Adopting must never make it Bedouin's to remove. /cfg is mounted read-only,
# and `add` edits the config, so this half works on a writable copy.
mkdir -p /tmp/cfg && cp /cfg/bedouin.yaml /tmp/cfg/
ADD=$(/bedouin --config /tmp/cfg/bedouin.yaml pickup \
  | sed -n 's/.*bedouin add \([a-z]*:tree\).*/\1/p' | head -1)
[ -n "$ADD" ] || { echo "FAIL: pickup printed no add line for tree"; exit 1; }
/bedouin --config /tmp/cfg/bedouin.yaml add "$ADD" --no-apply >/dev/null
/bedouin --config /tmp/cfg/bedouin.yaml apply -y >/dev/null
grep -A3 '"package/tree"' "$HOME/.local/state/bedouin/state.json" | grep -q '"owner": "preexisting"' \
  || { echo "FAIL: bedouin claimed a package it did not install"; exit 1; }
echo "OK: adopted as preexisting, not claimed ($ADD)"
