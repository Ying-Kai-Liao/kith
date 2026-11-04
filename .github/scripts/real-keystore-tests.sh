#!/usr/bin/env bash
# Runs the secrets envelope's real-store tests (core/runtime/src/secrets/tests.rs, gated behind
# KITH_TEST_OS_KEYSTORE=1) against a throwaway store, so nothing touches a real login keychain:
#   macOS: a keychain made in this job and set as the default;
#   Linux: gnome-keyring in its own D-Bus session, unlocked with a throwaway password.
# Run from serve/host. Needs the pinned toolchain and, on Linux, gnome-keyring, libsecret-tools and dbus.
set -euo pipefail

run_tests() {
  local out
  out="$(KITH_TEST_OS_KEYSTORE=1 cargo test -p kith-core --locked os_store_ -- --nocapture --test-threads=1 2>&1)" || {
    echo "$out"
    return 1
  }
  echo "$out"
  # The tests return early without the variable; make sure one really ran against the store.
  echo "$out" | grep -Eq 'test .*os_store_(macos_security_cli|linux_secret_tool) \.\.\. ok' || {
    echo "::error::the real-store test did not run"
    return 1
  }
}

case "$(uname -s)" in
  Darwin)
    kc="${RUNNER_TEMP:-/tmp}/kith-test-$$.keychain-db"
    old_default="$(security default-keychain -d user | tr -d ' "')"
    old_list="$(security list-keychains -d user | tr -d ' "')"
    cleanup() {
      # shellcheck disable=SC2086
      security list-keychains -d user -s $old_list || true
      security default-keychain -d user -s "$old_default" || true
      security delete-keychain "$kc" || true
    }
    trap cleanup EXIT
    security create-keychain -p throwaway "$kc"
    security set-keychain-settings -lut 3600 "$kc"
    security unlock-keychain -p throwaway "$kc"
    security list-keychains -d user -s "$kc"
    security default-keychain -d user -s "$kc"
    security default-keychain -d user
    run_tests
    ;;
  Linux)
    # Build first, outside the session, so the session only runs the tests.
    cargo test -p kith-core --locked --no-run
    export -f run_tests
    dbus-run-session -- bash -c '
      set -euo pipefail
      echo -n throwaway | gnome-keyring-daemon --unlock --components=secrets >/dev/null
      run_tests
    '
    ;;
  *)
    echo "no real key store test for $(uname -s)" >&2
    exit 1
    ;;
esac
