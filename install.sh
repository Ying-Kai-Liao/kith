#!/usr/bin/env bash
# Set up Kith on this Mac: the kith command (and hq, its old name), the Claude Code plugin, then `kith init` (your
# folders, backups, a starter agent). Safe to run again.
#
# The kith command is one versioned release copy under ~/.kith/cli/ (scripts/install.sh installs it, the Mac app
# installs it too); this file, packaged inside that release as share/kith/install.sh, links it into ~/.local/bin and
# does the rest. Run from a git checkout it does nothing by default: a checkout is for developing kith, and the command
# on your PATH never silently follows it. Use it with KITH_KERNEL=<checkout>/kernel, or `./install.sh --dev` to link the
# checkout's own command and plugin (the old behaviour, an explicit choice).
#
#   curl -fsSL https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install.sh | sh
#   curl -fsSL .../scripts/install.sh | sh -s -- --from <your-github-user>      a new Mac: brings back your state and agents
#
# ~/kith is the Kith folder: agents/ (your agents, their own private repo). ~/.kith is this Mac's Kith: its knowledge
# (user.md, projects, agents.json; your hq-state repo), machine-only things (secrets/, job state, logs) and the cli/
# folder of releases. See docs/architecture.md.
#
# A Mac that ran hq keeps working untouched: ~/.hq stays its state until `kith migrate` moves it to ~/.kith, an
# existing ~/kith or ~/hq (a link or a folder) is left as it is, and the hq command still works.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd -P)"
KERNEL=$ROOT/kernel
CUSTOM_HOME=${KITH_HOME:-${HQ_HOME:-}}   # set by you, before the resolver fills KITH_HOME in
DEV=
ARGS=()
for a in "$@"; do
  if [ "$a" = --dev ]; then DEV=1; else ARGS+=("$a"); fi
done

# A release (from scripts/install.sh or the Mac app) sits in <machine>/cli/<version>/share/kith, with bin/kith beside
# share/. Its stable address is the same folder through cli/current, which is what links, plists and the plugin get.
PACKAGED=
VER_DIR=$(cd "$ROOT/../.." 2>/dev/null && pwd -P || true)   # a real path (pwd -P)
if [ -x "$ROOT/../../bin/kith" ] && [ ! -d "$ROOT/serve/host" ]; then
  PACKAGED=1
  # <machine>/cli/<version>/share/kith: current is beside the version. An older flat install is <prefix>/share/kith.
  if [ "$(basename "$(dirname "$VER_DIR")")" = cli ]; then
    STABLE=$(dirname "$VER_DIR")/current
    ROOT=$STABLE/share/kith
    KERNEL=$ROOT/kernel
  fi
fi
if [ -z "$PACKAGED" ] && [ -z "$DEV" ]; then
  cat >&2 <<NOTE
This is a git checkout of kith ($ROOT), for developing it. The kith command on your PATH is the installed release,
never a checkout:
  install it:        curl -fsSL https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install.sh | sh
  develop on kith:   KITH_KERNEL=$ROOT/kernel kith ...
  or link this checkout as the command and plugin (explicit; \`kith doctor\` warns about it): $0 --dev
NOTE
  exit 0
fi
mkdir -p "$HOME/.local/bin"
chmod +x "$KERNEL/bin/"* "$KERNEL/lib/kith_paths.py" 2>/dev/null || true

# ~/kith is Kith's stable address: a real folder holding agents/, so tools and agents never need to know where it really
# is. An existing ~/kith (a folder, or a link to a clone or an external disk) and ~/hq are left as they are. KITH_HOME
# points elsewhere instead (kernel/lib/kith_paths.py). A --dev run from a checkout keeps linking ~/kith to it.
link_home() {   # link_home NAME: ~/NAME -> this folder, unless it is something else
  if [ ! -e "$HOME/$1" ] && [ ! -L "$HOME/$1" ]; then ln -s "$ROOT" "$HOME/$1"
  elif [ "$(cd "$HOME/$1" && pwd -P)" != "$ROOT" ]; then return 1
  fi
}
if [ -z "$CUSTOM_HOME" ]; then
  if [ -n "$PACKAGED" ]; then
    [ -e "$HOME/kith" ] || [ -L "$HOME/kith" ] || mkdir -p "$HOME/kith/agents"
  else
    # a clone made at ~/kith is its own address; ~/hq is still linked to it for the tools that use the old name
    [ "$ROOT" = "$HOME/kith" ] || link_home kith || { echo "install: ~/kith already exists and is not this folder: move it, or set KITH_HOME to use another folder" >&2; exit 1; }
    link_home hq || echo "note: ~/hq is another folder; ~/kith is the address Kith uses"
  fi
fi
# ~/.kith is a real folder on this Mac (kith init fills it). A Mac that ran hq has ~/.hq instead and keeps using it
# until `kith migrate`; before 2026-10-15 ~/.hq was a link to the Kith folder: that layout keeps working too (state in
# <kith>/state, machine things in ~/.config/hq).
if [ -L "$HOME/.hq" ]; then
  echo "note: ~/.hq is a link (the older layout); kith keeps using <kith>/state and ~/.config/hq until you move them"
elif [ -d "$HOME/.hq" ]; then
  echo "note: this Mac's state is still ~/.hq; kith migrate moves it to ~/.kith (everything works until then)"
else
  mkdir -p "$HOME/.kith/secrets" && chmod 700 "$HOME/.kith/secrets"
  touch "$HOME/.kith/.migrated"   # ~/.kith is this Mac's folder (lib/kith_paths.py reads the marker)
fi

if [ -n "$PACKAGED" ]; then
  if [ -n "${STABLE:-}" ]; then
    CMD=$STABLE/bin/kith   # cli/current: an update moves the link, ~/.local/bin/kith stays
  else
    CMD="$(cd "$ROOT/../../bin" && pwd -P)/kith"   # a flat install of before
  fi
else
  # Through ~/kith: the path agents see then has no volume name (and no spaces) in it.
  CMD=$KERNEL/bin/kith; [ -z "$CUSTOM_HOME" ] && CMD=$HOME/kith/kernel/bin/kith
fi
ln -sfn "$CMD" "$HOME/.local/bin/kith"
ln -sfn "$CMD" "$HOME/.local/bin/hq"
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo "note: add ~/.local/bin to your PATH to use kith (e.g. in ~/.zshrc: export PATH=\"\$HOME/.local/bin:\$PATH\")" ;; esac

if command -v claude >/dev/null; then
  claude plugin marketplace add "$ROOT" >/dev/null 2>&1 || true
  claude plugin install kith@kith >/dev/null 2>&1 || echo "note: the Claude Code plugin did not install: claude plugin install kith@kith"
  if claude plugin list 2>/dev/null | grep -q 'hq@hq'; then
    echo "note: the old plugin hq@hq is still installed (subagents hq:<agent>): kith migrate removes it, or claude plugin uninstall hq@hq"
  fi
  [ -z "$CUSTOM_HOME$DEV" ] || echo "note: set the plugin's kith_path to $KERNEL in /plugin"
fi

# Kith's host is optional and takes minutes to build, so it is never built here.
if [ -d "$ROOT/serve/host" ] && { command -v cargo >/dev/null || ls "$HOME"/.rustup/toolchains/*/bin/cargo >/dev/null 2>&1; }; then
  echo "note: kith serve start builds and runs Kith's host on this Mac (a few minutes the first time)"
fi

# Your folders, backups and a first agent; --from <user> brings back yours from GitHub.
exec "$KERNEL/bin/kith-init" ${ARGS[@]+"${ARGS[@]}"}
