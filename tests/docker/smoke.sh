#!/bin/sh
# Layer 3 (spec §11): a real apply against a real package manager on a real
# distro. Slow and few -- it exists to catch the lies in FakeHost's fakes.
set -eu
echo "== plan =="
/bedouin --config /cfg/bedouin.yaml plan || [ $? -eq 2 ]
echo "== apply =="
/bedouin --config /cfg/bedouin.yaml apply -y
echo "== assertions =="
command -v jq >/dev/null || { echo "FAIL: jq was not installed"; exit 1; }
grep -q 'editor = nano' "$HOME/.gitconfig" || { echo "FAIL: template did not render"; exit 1; }
grep -q 'JQ_EDITOR=nano' "$HOME/.bashrc.d/70-jq.bash" || { echo "FAIL: rc block missing"; exit 1; }
grep -q 'bedouin: source' "$HOME/.bashrc" || { echo "FAIL: rc file not wired to the drop-in dir"; exit 1; }
grep -q '.local/bin' "$HOME/.bashrc.d/00-bedouin-path.bash" || { echo "FAIL: PATH file missing"; exit 1; }
# The rc file must actually work when a shell reads it.
sh -c ". $HOME/.bashrc.d/70-jq.bash; [ \"\$JQ_EDITOR\" = nano ]" || { echo "FAIL: rc block is not valid shell"; exit 1; }
echo "== converges? =="
/bedouin --config /cfg/bedouin.yaml plan
/bedouin --config /cfg/bedouin.yaml plan >/dev/null && echo "OK: second plan exits 0"
