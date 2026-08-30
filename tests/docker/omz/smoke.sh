#!/bin/sh
set -eu
echo "== plan =="
/bedouin --config /cfg/bedouin.yaml plan || [ $? -eq 2 ]
echo "== apply =="
/bedouin --config /cfg/bedouin.yaml apply -y >/tmp/apply.log 2>&1 || { tail -20 /tmp/apply.log; exit 1; }
tail -1 /tmp/apply.log
echo "== assertions =="
[ -d "$HOME/.oh-my-zsh" ] || { echo "FAIL: omz not installed"; exit 1; }
command -v zsh >/dev/null || { echo "FAIL: zsh not installed"; exit 1; }
[ -d "$HOME/.oh-my-zsh/custom/plugins/zsh-autosuggestions" ] || { echo "FAIL: plugin repo not cloned"; exit 1; }
grep -q "bedouin: framework" "$HOME/.zshrc" || { echo "FAIL: no framework block"; exit 1; }
grep -q "ZSH_THEME='agnoster'" "$HOME/.zshrc" || { echo "FAIL: theme not set"; exit 1; }

# The whole point: the block must precede the line that reads it.
blk=$(grep -n "bedouin: framework" "$HOME/.zshrc" | head -1 | cut -d: -f1)
src=$(grep -n "oh-my-zsh.sh" "$HOME/.zshrc" | head -1 | cut -d: -f1)
[ -n "$src" ] || { echo "FAIL: no omz loader line"; exit 1; }
[ "$blk" -lt "$src" ] || { echo "FAIL: block at $blk is AFTER loader at $src"; exit 1; }
echo "  framework block at line $blk, omz loader at line $src -- correct order"

# And zsh must actually reach the end of the file with the theme applied.
zsh -i -c 'echo "  theme in a live shell: $ZSH_THEME"; echo "  plugins: ${plugins[*]}"' </dev/null
echo "== converges =="
/bedouin --config /cfg/bedouin.yaml plan
/bedouin --config /cfg/bedouin.yaml doctor
echo OK
