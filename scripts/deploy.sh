#!/usr/bin/env bash
# Deploy the remote connector to Fly.io with a LEAST-PRIVILEGE, app-scoped
# deploy token — never the broad personal/org session from `fly auth login`.
#
# Why: a deploy carries full control of the app, including the power to set
# MCP_AUTH_DISABLED=1 and reopen the connector to anyone. Scoping the deploy
# credential to this ONE app (and giving it a short expiry) means a leaked
# token can't touch the rest of your Fly org, and it ages out on its own.
# See docs/REMOTE-CONNECTOR.md §"Least-privilege deploy tokens".
#
# Token resolution order:
#   1. $FLY_API_TOKEN in the environment (CI, or your password manager export)
#   2. ./.fly.deploy.token (gitignored; created on first run)
# The script NEVER prints the token. Extra args are passed through to
# `fly deploy` (e.g. ./scripts/deploy.sh --build-only).
set -euo pipefail

cd "$(dirname "$0")/.."

if [[ ! -f fly.toml ]]; then
  echo "✖ fly.toml not found. Copy fly.toml.example → fly.toml and set your app name." >&2
  exit 1
fi
APP="$(awk -F'\"' '/^app[[:space:]]*=/{print $2; exit}' fly.toml)"
TOKEN_FILE=".fly.deploy.token"
# Token lifetime. Short on purpose (least privilege); re-mint when it expires.
TOKEN_EXPIRY="${FLY_TOKEN_EXPIRY:-720h}"   # 30 days

# Resolve the token without ever echoing it.
if [[ -n "${FLY_API_TOKEN:-}" ]]; then
  echo "→ Using app-scoped deploy token from \$FLY_API_TOKEN."
elif [[ -f "$TOKEN_FILE" ]]; then
  echo "→ Using app-scoped deploy token from $TOKEN_FILE."
  FLY_API_TOKEN="$(cat "$TOKEN_FILE")"
else
  echo "→ No deploy token found. Minting an app-scoped one for '$APP' (expiry $TOKEN_EXPIRY)…"
  echo "  (This step uses your interactive Fly login once; the deploy itself does not.)"
  umask 077
  fly tokens create deploy -a "$APP" --name "deploy-$APP" --expiry "$TOKEN_EXPIRY" \
    > "$TOKEN_FILE"
  chmod 600 "$TOKEN_FILE"
  FLY_API_TOKEN="$(cat "$TOKEN_FILE")"
  echo "✔ Wrote $TOKEN_FILE (gitignored, mode 600). Back it up in your password manager."
fi
export FLY_API_TOKEN

echo "→ Deploying '$APP' with the scoped token…"
exec fly deploy -a "$APP" "$@"
