# Web client hosting

The member web client (`npm run build:web` -> `dist-web/`) is served by an assets-only Cloudflare Worker, `kith-web-dev`
(https://kith-web-dev.lykevin890919.workers.dev). No Worker script, no bindings, no routes. `wrangler.jsonc` is the
config, `_headers` the strict CSP and cache headers, `check.sh` the local check:

    cd apps/desktop && sh web-host/check.sh   # build, stage _headers, verify script-src 'self', wrangler deploy --dry-run

The Worker is deployed only by `.github/workflows/web-deploy.yml` (push to main touching `apps/desktop/**`, or a manual
run on main). Each deploy publishes `manifest.json` (sha256 of every served file + the commit) as a workflow artifact and
on the rolling prerelease `web-dev`, so what is served can be compared with what CI built, outside Cloudflare.
Until the secrets below exist the deploy job prints a notice and skips; the build job still runs the check.

## One-time steps (Kai)

1. Cloudflare dashboard -> Manage Account -> Account API tokens -> Create Token -> Custom. One permission:
   Account / Workers Scripts / Edit. Account resources: only your account. Add a Client IP filter or a short expiry if you like.
2. Turn on two-factor authentication for the Cloudflare login.
3. GitHub repo -> Settings -> Environments -> New environment `kith-web-dev`:
   - Required reviewers: you.
   - Deployment branches: Selected branches -> `main`.
   - Environment secrets: `CLOUDFLARE_API_TOKEN` (the token) and `CLOUDFLARE_ACCOUNT_ID` (Cloudflare dashboard -> Workers & Pages -> Account ID).
4. Clerk dashboard (dev instance): allow `https://kith-web-dev.lykevin890919.workers.dev` as a redirect origin.
5. Never run `wrangler login` on the agent Macs; the token lives only in that GitHub environment.

Docs used: https://developers.cloudflare.com/workers/static-assets/ , .../static-assets/headers/ ,
.../static-assets/routing/single-page-application/ , .../workers/ci-cd/external-cicd/github-actions/ .
