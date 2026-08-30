#!/bin/sh
set -eu
echo "== facts =="
/bedouin --config /cfg/bedouin.yaml facts | grep -E '"(distro|distro_like|privilege)"'
echo "== apply =="
/bedouin --config /cfg/bedouin.yaml apply -y
echo "== assertions =="
command -v jq >/dev/null || { echo "FAIL: jq not installed"; exit 1; }
grep -q "name = root" "$HOME/.gitconfig" || { echo "FAIL: template"; exit 1; }
grep -q "alias j='jq'" "$HOME/.bashrc.d/30-jq-aliases.bash" || { echo "FAIL: package aliases"; exit 1; }
grep -q "alias ll=" "$HOME/.bashrc.d/10-bedouin-aliases.bash" || { echo "FAIL: global aliases"; exit 1; }
sh -c ". $HOME/.bashrc.d/10-bedouin-aliases.bash" || { echo "FAIL: aliases not valid shell"; exit 1; }
echo "== doctor + converge =="
/bedouin --config /cfg/bedouin.yaml doctor
/bedouin --config /cfg/bedouin.yaml plan
echo "OK"
