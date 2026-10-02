#!/bin/sh
# `bedouin ssh` cloning a PRIVATE repository through a forwarded agent, with no
# GitHub involved: one container is a git server that accepts a single key, the
# other is the machine being provisioned. Needs docker, ssh and ssh-agent.
# Usage: tests/sshprivate.sh path/to/bedouin
set -eu
B=$(realpath "$1"); T=$(mktemp -d); N=bedsshtest$$
cleanup() { docker rm -f $N-git $N-tgt >/dev/null 2>&1; docker network rm $N >/dev/null 2>&1
            docker rmi -f $N-git $N-tgt >/dev/null 2>&1
            [ -n "${SSH_AGENT_PID:-}" ] && ssh-agent -k >/dev/null 2>&1; rm -rf "$T"; }
trap cleanup EXIT
cd "$T"
ssh-keygen -q -t ed25519 -N '' -f login; ssh-keygen -q -t ed25519 -N '' -f gitkey
mkdir git target
cp gitkey.pub git/authorized_keys; cp login.pub target/authorized_keys
cat > git/Dockerfile <<'EOF'
FROM alpine:3
RUN apk add --no-cache openssh git && ssh-keygen -A && adduser -D -s /bin/sh git && passwd -u git \
 && mkdir -p /home/git/.ssh && git init -q --bare -b main /srv/p.git && chown -R git /srv /home/git
COPY authorized_keys /home/git/.ssh/authorized_keys
RUN chown git /home/git/.ssh/authorized_keys && chmod 600 /home/git/.ssh/authorized_keys && chmod 700 /home/git/.ssh
CMD ["/usr/sbin/sshd","-D","-e"]
EOF
cat > target/Dockerfile <<'EOF'
FROM ubuntu:24.04
RUN apt-get update -qq && apt-get install -y -qq openssh-server curl ca-certificates >/dev/null \
 && ssh-keygen -A && mkdir -p /run/sshd /root/.ssh
COPY authorized_keys /root/.ssh/authorized_keys
RUN chmod 700 /root/.ssh && chmod 600 /root/.ssh/authorized_keys
CMD ["/usr/sbin/sshd","-D","-e"]
EOF
docker network create $N >/dev/null
docker build -q -t $N-git git >/dev/null; docker build -q -t $N-tgt target >/dev/null
docker run -d --name $N-git --network $N $N-git >/dev/null
docker run -d --name $N-tgt --network $N -p 127.0.0.1::22 $N-tgt >/dev/null
PORT=$(docker port $N-tgt 22/tcp | head -1 | sed 's/.*://')
sleep 2
# The binary under test is also the one on the machine, so the remote half of
# the walk (`bedouin sync` with its options) is this build and not whatever the
# last release happens to be. It also keeps the test off the network for the
# install stage, which then correctly finds bedouin already there.
docker exec $N-tgt mkdir -p /root/.local/bin && docker cp "$B" $N-tgt:/root/.local/bin/bedouin
docker exec -u git $N-git sh -c 'cd /tmp && git clone -q /srv/p.git w 2>/dev/null; cd w \
  && git config user.email a@b && git config user.name t \
  && printf "version: 0\nshell: bash\npackages:\n  - {name: jq, from: apt}\n" > bedouin.yaml \
  && git add -A && git commit -qm seed && git push -q origin HEAD:main'
printf 'version: 0\nshell: bash\npackages:\n  - {name: jq, from: apt}\n' > cfg.yaml

prov() { "$B" --config cfg.yaml ssh root@127.0.0.1 --repo git@$N-git:/srv/p.git -y \
  -- -p "$PORT" -i "$T/login" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR 2>&1; }
fresh() { docker exec $N-tgt sh -c 'rm -rf /root/.config/bedouin /root/.ssh/known_hosts'; }
logins() { docker logs $N-tgt 2>&1 | grep -c "Accepted publickey" || true; }

echo "== no agent: refused with a sentence about the agent"
fresh
out=$(env -u SSH_AUTH_SOCK "$B" --config cfg.yaml ssh root@127.0.0.1 --repo git@$N-git:/srv/p.git -y \
  -- -p "$PORT" -i "$T/login" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR 2>&1) \
  && { echo "FAIL: provisioned with no agent"; exit 1; } || true
echo "$out" | grep -qE "no ssh agent|holds no keys" || { echo "FAIL: no explanation:"; echo "$out"; exit 1; }
echo "OK"

echo "== an agent whose key the repo does not accept: says whose key has to be allowed"
fresh; eval "$(ssh-agent -s)" >/dev/null; ssh-add -q login
out=$(prov) && { echo "FAIL: cloned with the wrong key"; exit 1; } || true
echo "$out" | grep -q "ssh-add -l" || { echo "FAIL: no hint:"; echo "$out"; exit 1; }
ssh-agent -k >/dev/null; unset SSH_AGENT_PID
echo "OK"

echo "== the right key: clones the private repo, applies, and logs in ONCE"
fresh; eval "$(ssh-agent -s)" >/dev/null; ssh-add -q gitkey
before=$(logins); out=$(prov) || { echo "FAIL:"; echo "$out"; exit 1; }
n=$(( $(logins) - before ))
echo "$out" | grep -q "Provisioned" || { echo "FAIL: not provisioned"; echo "$out"; exit 1; }
docker exec $N-tgt test -f /root/.config/bedouin/bedouin.yaml || { echo "FAIL: nothing cloned"; exit 1; }
[ "$n" = 1 ] || { echo "FAIL: $n logins for one run; the stages are not sharing a connection"; exit 1; }
echo "OK: 1 login"

echo "== options reach the remote stages; a flag beats the environment"
# State and jq are cleared so the apply has something to skip. The environment
# says to skip something that does not exist; the flag says package/jq. If jq is
# absent afterwards the flag won. --depth 1 reaches `git clone` the same way.
fresh; docker exec $N-tgt sh -c 'rm -rf /root/.local/state/bedouin; apt-get remove -y -qq jq >/dev/null 2>&1 || true'
eval "$(ssh-agent -s)" >/dev/null; ssh-add -q gitkey
out=$(BEDOUIN_CLONE_OPTIONS="--depth 1" BEDOUIN_APPLY_OPTIONS="--skip package/nothing" \
  "$B" --config cfg.yaml ssh root@127.0.0.1 --repo git@$N-git:/srv/p.git -y --apply-options "--skip package/jq" \
  -- -p "$PORT" -i "$T/login" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR 2>&1) \
  || { echo "FAIL:"; echo "$out"; exit 1; }
docker exec $N-tgt test -f /root/.config/bedouin/.git/shallow || { echo "FAIL: --depth 1 did not reach git clone"; exit 1; }
if docker exec $N-tgt sh -c 'command -v jq >/dev/null'; then echo "FAIL: --skip package/jq did not reach sync (or lost to the environment)"; exit 1; fi
echo "OK: clone options, apply options, flag over environment"
