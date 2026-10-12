#!/usr/bin/env bash
# Upload the Worker's secrets from the environment, as encrypted secrets
# (never plain-text variables, which `wrangler deploy` and Workers Builds
# remove). Values are piped, so they appear in no command line or log.
#
#   H5I_APP_SECRET=...  CLOUDFLARE_API_TOKEN=... (Workers Scripts: Edit) \
#     scripts/worker-secrets.sh
#
# Afterwards `npx wrangler secret list --name o5s` shows each as secret_text.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${H5I_APP_SECRET:?set H5I_APP_SECRET to the token signing key}"
: "${CLOUDFLARE_API_TOKEN:?set CLOUDFLARE_API_TOKEN to a token with Workers Scripts: Edit}"
file="$(mktemp)"
trap 'rm -f "$file"' EXIT
chmod 600 "$file"
# JSON, so a value cannot carry a stray newline into the secret.
python3 -c 'import json, os; print(json.dumps({"H5I_APP_SECRET": os.environ["H5I_APP_SECRET"]}))' > "$file"
npx --yes wrangler@4 secret bulk "$file" --name o5s
npx --yes wrangler@4 secret list --name o5s
