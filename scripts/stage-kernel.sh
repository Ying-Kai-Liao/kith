#!/bin/sh
# Copies the kernel tree and its companions into <share-dir> (the share/kith of an install):
#
#   scripts/stage-kernel.sh <share-dir> [version]
#
# The one place that lists what a packaged kith carries beside its binary. package-release.sh (the release
# tarball) and the Mac app's build (apps/desktop/tools/native-host.mjs) both call it, so they cannot drift.
# It stamps kernel/VERSION (one line, the release version: [version], else serve/host/Cargo.toml's): the installer
# files a release under cli/<version>/, and the host reads the CLI's version from it. A checkout has no VERSION file.
# With `windows` as the third argument (the Windows zip, package-windows.sh) only the data is staged: no kernel/bin and no
# kernel/lib (the Python and shell programs) and no POSIX install.sh.
set -eu
[ $# -ge 1 ] && [ $# -le 3 ] || { sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
S=$1
WINDOWS=
[ "${3:-}" = windows ] && WINDOWS=1
ROOT=$(cd "$(dirname "$0")/.." && pwd -P)
VERSION=${2:-$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$ROOT/serve/host/Cargo.toml" | head -1)}
VERSION=${VERSION#v}
[ -n "$VERSION" ] || { echo "stage-kernel: no version (pass one, or serve/host/Cargo.toml has none)" >&2; exit 1; }
mkdir -p "$S"
if [ -n "$WINDOWS" ]; then
  # What the Windows zip leaves out compared with the Mac tarball, and what stands in (docs/plans/windows-owner.md 4.11,
  # docs/plans/retire-python.md). Later packages must not assume these files exist on Windows. A verb with no stand-in
  # answers "not on Windows yet: <verb> (retire-python Pn-m)" from the W17 table; there is no bash fallback.
  #
  # kernel/bin (the router and every script; kernel/bin also holds symlinks Git Bash cannot create):
  #   kith                                  W17 Windows router (kith.exe); the bash is P5-1/P5-2
  #   project-id                            bin\project-id.exe (W17)
  #   kith-room-ledger                      never runs: the hook is always the program (W9); stub deleted P5-7
  #   kith-serve                            W11 (serve verbs in the host); bash port P4-8
  #   kith-serve-launch                     unused on Windows (kith-servew); Mac only, P1-17
  #   kith-auth, hq-auth                    nothing on Windows yet (P3-1, P3-2, P3-4)
  #   kith-allow                            nothing on Windows yet (P3-5)
  #   kith-cards                            nothing on Windows yet (P3-6)
  #   kith-send-to                          nothing on Windows yet (P3-7)
  #   kith-recall, kith-memory              nothing on Windows yet (P3-8, P3-9)
  #   kith-info                             nothing on Windows yet (P3-10)
  #   kith-people                           nothing on Windows yet (P3-11)
  #   kith-door                             guest-only, refused on Windows (P3-12)
  #   kith-pkg, hq-pkg                      nothing on Windows yet (P4-1; share/push/commit/search/market P4-2)
  #   kith-skills                           nothing on Windows yet (P4-3)
  #   kith-scratch                          nothing on Windows yet (P4-4)
  #   kith-project                          nothing on Windows yet (P4-5)
  #   kith-group                            nothing on Windows yet (P4-6)
  #   kith-connector                        nothing on Windows yet (P4-7)
  #   kith-send, hq-send                    nothing on Windows yet (P4-9, P4-24)
  #   kith-call                             nothing on Windows yet (P4-10)
  #   kith-new, hq-new                      nothing on Windows yet (P4-11)
  #   kith-doctor, hq-doctor                nothing on Windows yet (P4-12)
  #   kith-init, hq                         nothing on Windows yet (P4-13)
  #   kith-state, hq-state                  nothing on Windows yet (P4-14; the W11 timer replaces dev.kith.backup)
  #   kith-self-update                      nothing on Windows yet (P4-15; `kith update` is W15)
  #   kith-keys, kith-who, hq-who           nothing on Windows yet (P4-16)
  #   kith-bug                              nothing on Windows yet (P4-17)
  #   kith-chain                            nothing on Windows yet (P4-18)
  #   kith-helper-allow                     nothing on Windows yet (P4-19)
  #   kith-remote                           nothing on Windows yet (P4-20)
  #   kith-acp                              nothing on Windows yet (P4-21)
  #   kith-migrate                          nothing on Windows yet (P4-22: the hq-to-kith rename steps; cutover verbs are P0-7, done)
  #   kith-permit                           nothing on Windows yet (Python remainder: bug P4-17, door P3-12, guest helper)
  #   kith-wake                             nothing on Windows yet (P2-2)
  #   kith-ask                              nothing on Windows yet (P2-3)
  #   kith-sched                            nothing on Windows yet (P2-5, P2-6)
  #   kith-sessions                         nothing on Windows yet (P1-19); `kith kernel threads` is the Rust path
  #
  # kernel/lib:
  #   kith-core.sh, core-switched.txt       nothing on Windows yet (P5-2, with the bash router)
  #   requires-switched.txt                 unowned (the installer gate reads it, P0-14; Windows installs via W14)
  #   kith-env.sh, guest-env.sh             unowned
  #   kith_paths.py                         `kith kernel paths` (P1-7, done); deleted P5-3
  #   kith_settings.py                      `kith kernel settings` for the set verbs; reads P1-13; deleted P5-3
  #   kith_accounts.py                      `kith account`; chain verbs nothing on Windows yet (P1-14); deleted P5-3
  #   kith_guard.py                         Rust launch/judge.rs (P1-8, done); deleted P5-3
  #   kith_readfence.py                     `kith kernel readfence`; deleted P1-18
  #   kith_lifecycle.py                     `kith kernel lifecycle` (P0-8, done); deleted P1-19
  #   kith_claims.py, kith_registry.py, kith_procstart.py   core claims; deleted P2-7
  #   kith_sched_core.py                    nothing on Windows yet (P2-6)
  #   kith_allowed.py                       nothing on Windows yet (P3-5)
  #   kith_recall.py, kith_runsocket.py     nothing on Windows yet (P3-8)
  #   kith_people_access.py, kith_host.py   nothing on Windows yet (P3-11)
  #   kith_door.py, kith_door_core.py, door-chrome, door-preload.mjs   guest-only, refused on Windows (P3-12)
  #   kith_trajectory.py                    nothing on Windows yet (P4-4)
  #   kith_launchd.py                       Mac only; goes with P4-14
  #   kith_bug.py, kith_redact.py           nothing on Windows yet (P4-17)
  #   kith_chain.py                         nothing on Windows yet (P4-18)
  #   kith_migrate.py                       nothing on Windows yet (P4-22)
  #   kith_facets.py, kith_jobs.py, kith_runenv.py   no verb of their own (the Rust launch replaces them); deleted P5-3
  #   kith_tombstone.py                     core/runtime cutover/tombstone.rs writes it; no Python reader on Windows; deleted P5-3
  #   starters.json, user.md, agents.gitignore, dot-kith.gitignore   data, not staged: P5-4 moves them to share/kith/
  #
  #   kernel/tests the repo's tests.
  #   install.sh   the POSIX installer; Windows uses scripts/install.ps1 (W14).
  # Also never copy kernel/bin: it holds symlinks (hq -> kith-*) that Git Bash on Windows cannot create.
  mkdir -p "$S/kernel"
  for d in "$ROOT"/kernel/*; do
    case $(basename "$d") in bin | lib | tests) ;; *) cp -R "$d" "$S/kernel/" ;; esac
  done
else
  cp -R "$ROOT/kernel" "$S/kernel"
fi
cp -R "$ROOT/.claude-plugin" "$S/.claude-plugin"
cp "$ROOT/LICENSE" "$ROOT/README.md" "$S/"
[ -n "$WINDOWS" ] || cp "$ROOT/install.sh" "$S/"
# The NOTICE has to travel with every redistribution of the host binary.
cp "$ROOT/NOTICE" "$ROOT/THIRD_PARTY_NOTICES.md" "$S/"
# Tests and caches are for the repo, not the install.
rm -rf "$S/kernel/tests"
[ -z "$WINDOWS" ] || rm -rf "$S/kernel/bin" "$S/kernel/lib"
printf '%s\n' "$VERSION" > "$S/kernel/VERSION"
find "$S" \( -name __pycache__ -o -name .DS_Store \) -prune -exec rm -rf {} +
