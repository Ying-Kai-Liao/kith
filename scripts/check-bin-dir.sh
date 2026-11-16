#!/bin/sh
# Fails when a packaged kith bin folder lacks a program every install needs beside kith: kith-stub (the conversation
# lock) and kith-holder (the process holder Writer mode connects to). kith-tts is optional (Apple silicon only).
#
#   scripts/check-bin-dir.sh <bin-dir>     the release tarball's bin/, the Mac app's Resources/kith/bin
set -eu
[ $# -eq 1 ] || { sed -n '2,5p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
missing=
for n in kith kith-stub kith-holder; do
  [ -f "$1/$n" ] && [ -x "$1/$n" ] || missing="$missing $n"
done
[ -z "$missing" ] || { echo "check-bin-dir: $1 lacks:$missing" >&2; exit 1; }
echo "check-bin-dir: $1 has kith, kith-stub and kith-holder"
