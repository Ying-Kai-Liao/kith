#!/usr/bin/env bash
# Set up Kith on this Mac: the kith command (and hq, its old name), the Claude Code plugin, then `kith init` (your
# folders, backups, a starter agent). Safe to run again.
#
#   git clone https://github.com/Ying-Kai-Liao/kith ~/kith && ~/kith/install.sh
#   ~/kith/install.sh --from <your-github-user>      a new Mac: brings back your state and agents
#
# This folder is the Kith folder: kernel/ is the code (this repo), and next to it, ignored by this repo, agents/ (your
# agents, their own private repo). ~/.kith is this Mac's Kith: its knowledge (user.md, projects, agents.json; your
# hq-state repo) and machine-only things (secrets/, job state, logs). See docs/architecture.md.
#
# A Mac that ran hq keeps working untouched: ~/.hq stays its state until `kith migrate run` moves it to ~/.kith, ~/hq stays
# a link to the same folder as ~/kith, and the hq command still works.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd -P)"
KERNEL=$ROOT/kernel
CUSTOM_HOME=${KITH_HOME:-${HQ_HOME:-}}   # set by you, before the resolver fills KITH_HOME in
mkdir -p "$HOME/.local/bin"
chmod +x "$KERNEL/bin/"* "$KERNEL/lib/kith_paths.py"

# ~/kith is Kith's stable address, a link to this folder, so tools and agents never need to know where it really is
# (an external disk, a path with spaces). ~/hq is the same link under the old name. KITH_HOME points elsewhere
# instead (kernel/lib/kith_paths.py).
link_home() {   # link_home NAME: ~/NAME -> this folder, unless it is something else
  if [ ! -e "$HOME/$1" ] && [ ! -L "$HOME/$1" ]; then ln -s "$ROOT" "$HOME/$1"
  elif [ "$(cd "$HOME/$1" && pwd -P)" != "$ROOT" ]; then return 1
  fi
}
if [ -z "$CUSTOM_HOME" ]; then
  # a clone made at ~/kith is its own address; ~/hq is still linked to it for the tools that use the old name
  [ "$ROOT" = "$HOME/kith" ] || link_home kith || { echo "install: ~/kith already exists and is not this folder: move it, or set KITH_HOME to use another folder" >&2; exit 1; }
  link_home hq || echo "note: ~/hq is another folder; ~/kith is the address Kith uses"
fi
# ~/.kith is a real folder on this Mac (kith init fills it). A Mac that ran hq has ~/.hq instead and keeps using it
# until `kith migrate run`; before 2026-10-15 ~/.hq was a link to the Kith folder: that layout keeps working too (state in
# <kith>/state, machine things in ~/.config/hq).
if [ -L "$HOME/.hq" ]; then
  echo "note: ~/.hq is a link (the older layout); kith keeps using <kith>/state and ~/.config/hq until you move them"
elif [ -d "$HOME/.hq" ]; then
  echo "note: this Mac's state is still ~/.hq; kith migrate run moves it to ~/.kith (everything works until then)"
else
  mkdir -p "$HOME/.kith/secrets" && chmod 700 "$HOME/.kith/secrets"
  touch "$HOME/.kith/.migrated"   # ~/.kith is this Mac's folder (lib/kith_paths.py reads the marker)
fi

# Through ~/kith: the path agents see then has no volume name (and no spaces) in it.
BIN=$KERNEL/bin/kith; [ -z "$CUSTOM_HOME" ] && BIN=$HOME/kith/kernel/bin/kith
# A packaged install (bin/kith beside share/kith/, from the release tarball) links the Rust binary instead; it answers
# to hq as well. Either way hq is only another name for the same command.
PACKAGED_KITH=$ROOT/../../bin/kith   # ROOT is a real path (pwd -P)
if [ -x "$PACKAGED_KITH" ] && [ ! -d "$ROOT/serve/host" ]; then
  CMD="$(cd "$(dirname "$PACKAGED_KITH")" && pwd -P)/kith"
else
  CMD=$BIN
fi
ln -sfn "$CMD" "$HOME/.local/bin/kith"
ln -sfn "$CMD" "$HOME/.local/bin/hq"
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo "note: add ~/.local/bin to your PATH to use kith (e.g. in ~/.zshrc: export PATH=\"\$HOME/.local/bin:\$PATH\")" ;; esac

if command -v claude >/dev/null; then
  claude plugin marketplace add "$ROOT" >/dev/null 2>&1 || true
  claude plugin install kith@kith >/dev/null 2>&1 || echo "note: the Claude Code plugin did not install: claude plugin install kith@kith"
  if claude plugin list 2>/dev/null | grep -q 'hq@hq'; then
    echo "note: the old plugin hq@hq is still installed (subagents hq:<agent>): kith migrate run removes it, or claude plugin uninstall hq@hq"
  fi
  [ -z "$CUSTOM_HOME" ] || echo "note: set the plugin's kith_path to $KERNEL in /plugin"
fi

# Kith's host is optional and takes minutes to build, so it is never built here.
if [ -d "$ROOT/serve/host" ] && { command -v cargo >/dev/null || ls "$HOME"/.rustup/toolchains/*/bin/cargo >/dev/null 2>&1; }; then
  echo "note: kith serve start builds and runs Kith's host on this Mac (a few minutes the first time)"
fi

# Your folders, backups and a first agent; --from <user> brings back yours from GitHub.
exec "$KERNEL/bin/kith-init" "$@"
