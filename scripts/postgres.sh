#!/usr/bin/env bash
# Start (or reuse) a local PostgreSQL 17 container for development and tests,
# and print the connection URL. Data lives in the container's volume.
# O5S_PG_NETWORK=host runs it on the host network (listening on 127.0.0.1
# only), without Docker's port proxy in every statement's path.
set -euo pipefail
name="${O5S_PG_CONTAINER:-o5s-dev-pg}"
port="${O5S_PG_PORT:-15439}"
password="${O5S_PG_PASSWORD:-o5s-dev}"
inner=5432
[[ "${O5S_PG_NETWORK:-}" == host ]] && inner="$port"
if ! docker inspect "$name" >/dev/null 2>&1; then
    if [[ "${O5S_PG_NETWORK:-}" == host ]]; then
        net=(--network host) args=(-c listen_addresses=127.0.0.1 -c "port=$port")
    else
        net=(-p "127.0.0.1:$port:5432") args=()
    fi
    docker run -d --name "$name" -e POSTGRES_USER=o5s -e POSTGRES_PASSWORD="$password" -e POSTGRES_DB=o5s \
        "${net[@]}" -v "$name-data:/var/lib/postgresql/data" postgres:17 "${args[@]}" >/dev/null
elif [[ "$(docker inspect -f '{{.State.Running}}' "$name")" != true ]]; then
    docker start "$name" >/dev/null
fi
for _ in $(seq 1 60); do
    docker exec "$name" pg_isready -p "$inner" -U o5s -d o5s >/dev/null 2>&1 && break
    sleep 1
done
echo "postgres://o5s:$password@127.0.0.1:$port/o5s"
