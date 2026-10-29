# Sourced by Claude Code before each Bash command of a guest's run (CLAUDE_ENV_FILE, set by bin/kith): their workspace
# (KITH_WORK) for the tools they run inside the OS sandbox. Never set for the run's process itself: the guest writes these
# folders, so nothing outside the sandbox may take a temp file, a cache or a program from them.
if [ -n "${KITH_WORK:-}" ]; then
  export TMPDIR="$KITH_WORK/tmp" XDG_CACHE_HOME="$KITH_WORK/cache" UV_CACHE_DIR="$KITH_WORK/cache/uv" \
    npm_config_cache="$KITH_WORK/cache/npm" PUPPETEER_CACHE_DIR="$KITH_WORK/cache/puppeteer" NODE_USE_ENV_PROXY=1
fi
