#!/bin/sh
# Layer 3 for the rhel-like family. dnf had install, remove and needs_root
# recipes and no test of any kind -- unit or container -- until this.
set -eu
echo "== facts =="
/bedouin --config /cfg/bedouin.yaml facts | grep -E '"(distro|distro_like|privilege)"'
# Fedora ships no ID_LIKE at all, so rhel-like rests on the fallback table.
/bedouin --config /cfg/bedouin.yaml facts | grep -q '"distro_like": "rhel"' \
  || { echo "FAIL: fedora did not land in the rhel family"; exit 1; }
echo "== apply =="
/bedouin --config /cfg/bedouin.yaml apply -y
echo "== assertions =="
command -v jq >/dev/null || { echo "FAIL: jq not installed by dnf"; exit 1; }
grep -q "name = root" "$HOME/.gitconfig" || { echo "FAIL: template"; exit 1; }
grep -q "alias j='jq'" "$HOME/.bashrc.d/30-jq-aliases.bash" || { echo "FAIL: package aliases"; exit 1; }
sh -c ". $HOME/.bashrc.d/10-bedouin-aliases.bash" || { echo "FAIL: aliases not valid shell"; exit 1; }
echo "== doctor + converge =="
/bedouin --config /cfg/bedouin.yaml doctor
/bedouin --config /cfg/bedouin.yaml plan
echo "OK"
