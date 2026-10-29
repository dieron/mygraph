#!/usr/bin/env bash
# Deploys the committed HEAD to a Docker host over SSH and (re)starts MyGraph with compose.
#
#   deploy/docker/deploy.sh <ssh-destination> [remote-dir]
#
#   ssh-destination  a Host alias from ~/.ssh/config, or user@host
#   remote-dir       the service's folder on the host, relative to its home (default: mygraph)
#
# First time: create <remote-dir>/.env on the host from .env.example and set the keys. The
# script never changes .env or the data volume. Before restarting it snapshots every graph of the
# running server (into the volume's snapshots/ folder), then rebuilds, restarts and waits until
# the server is healthy. Uncommitted changes are not deployed.
#
# MCP clients reach the server through the local `mygraph` program; update it too when the server
# gains tools (Windows: deploy/windows/deploy.ps1 -BridgeOnly).
set -euo pipefail

host=${1:?usage: deploy/docker/deploy.sh <ssh-destination> [remote-dir]}
dir=${2:-mygraph}
project=${MYGRAPH_COMPOSE_PROJECT:-mygraph}

cd "$(git rev-parse --show-toplevel)"
if [ -n "$(git status --porcelain)" ]; then
  echo "note: uncommitted changes are not deployed (HEAD only)" >&2
fi
commit=$(git rev-parse --short HEAD)
echo "deploying $commit to $host:$dir"

# 1. The source of HEAD, unpacked next to the running one ($dir is meant to expand here).
# shellcheck disable=SC2029
git archive --format=tar HEAD |
  ssh "$host" "set -e; mkdir -p '$dir'; rm -rf '$dir/.next'; mkdir '$dir/.next'; tar -xf - -C '$dir/.next'"

# 2. On the host: snapshot, swap the source, rebuild, restart, check.
ssh "$host" bash -s -- "$dir" "$project" "$commit" <<'REMOTE'
set -euo pipefail
dir=$1 project=$2 commit=$3
cd "$dir"
if [ ! -f .env ]; then
  echo "missing $dir/.env on the host: copy .env.example to .env and set the keys" >&2
  rm -rf .next
  exit 1
fi
setting() { sed -n "s/^$1=//p" .env | tail -1; }
key=$(setting MYGRAPH_ADMIN_KEY)
port=$(setting MYGRAPH_PORT); port=${port:-7420}
url="http://127.0.0.1:$port"
graphs() {
  curl -fsS -H "Authorization: Bearer $key" "$url/admin/graphs" | python3 -c '
import json, sys
for g in json.load(sys.stdin)["graphs"]:
    c = g.get("counts") or {}
    print(g["name"], g["status"], "nodes", c.get("nodes", "-"), "links", c.get("links", "-"),
          "last_seq", g.get("last_seq", "-"))'
}

if curl -fsS -o /dev/null "$url/health" 2>/dev/null; then
  for g in $(graphs | awk '$2 == "ready" { print $1 }'); do
    curl -fsS -X POST -H "Authorization: Bearer $key" "$url/admin/graphs/$g/snapshot" >/dev/null
    echo "snapshot: $g"
  done
fi

# The host's own files stay: .env, the commit marker and anything in import/.
rsync -a --delete --exclude=.env --exclude=.next --exclude=import --exclude=DEPLOYED_COMMIT .next/ ./
rm -rf .next
docker compose -p "$project" up -d --build 2>&1 | tail -3
echo "$commit" > DEPLOYED_COMMIT

container=$(docker compose -p "$project" ps -q mygraph)
status=starting
for _ in $(seq 1 60); do
  status=$(docker inspect -f '{{.State.Health.Status}}' "$container")
  [ "$status" = healthy ] && break
  sleep 2
done
echo "health: $status"
[ "$status" = healthy ] || { docker logs --tail 30 "$container"; exit 1; }
graphs
REMOTE
echo "deployed $commit"
