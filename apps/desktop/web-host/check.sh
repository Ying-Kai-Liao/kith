#!/bin/sh
# Builds dist-web, stages _headers, checks the result, and dry-runs the deploy (no credentials needed).
# Used locally and by web-deploy.yml, so CI deploys exactly what was checked.
set -eu
cd "$(dirname "$0")/.."
WRANGLER=4.152.0

npm run build:web
cp web-host/_headers dist-web/_headers

test -f dist-web/index.html || { echo "dist-web/index.html missing" >&2; exit 1; }
test -f dist-web/_headers || { echo "dist-web/_headers missing" >&2; exit 1; }
csp=$(grep -m1 'Content-Security-Policy:' dist-web/_headers)
script_src=$(printf '%s' "$csp" | sed 's/^ *Content-Security-Policy: *//' | tr ';' '\n' | sed 's/^ *//' | grep -E '^script-src ')
[ "$script_src" = "script-src 'self'" ] || { echo "script-src must be exactly 'self', got: $script_src" >&2; exit 1; }
printf '%s' "$csp" | grep -q "require-trusted-types-for 'script'" || { echo "trusted types missing" >&2; exit 1; }
# No inline script in the page: it would need 'unsafe-inline' and the CSP would block it.
if grep -Eq '<script[^>]*>[^<[:space:]]' dist-web/index.html; then echo "inline script in index.html" >&2; exit 1; fi

npx --yes wrangler@$WRANGLER deploy --dry-run --config web-host/wrangler.jsonc
echo "web-host check ok"
