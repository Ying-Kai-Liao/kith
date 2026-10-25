#!/bin/bash
# Boots one iPhone simulator, installs the Dev debug build and screenshots every launch mode in light and dark.
# CI only (.github/workflows/screens.yml); run from the repo root after the build.
# Usage: apps/ios/tools/screens.sh <derived-data-dir> <out-dir>
set -euo pipefail

dd="${1:?derived data dir}"
out="${2:?output dir}"
mkdir -p "$out"

app=$(find "$dd/Build/Products" -maxdepth 2 -name '*.app' -path '*DevDebug-iphonesimulator*' | head -n 1)
[ -n "$app" ] || { echo "no .app under $dd" >&2; exit 1; }
bundle=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Info.plist")

# A current iPhone: the newest "iPhone NN" that is not a Pro/Plus/Max/Air, else any iPhone.
udid=$(xcrun simctl list devices available -j | python3 -c '
import json, re, sys
devs = [d for runtime in json.load(sys.stdin)["devices"].values() for d in runtime if "iPhone" in d["name"]]
def rank(d):
    m = re.match(r"iPhone (\d+)$", d["name"])
    return (1, int(m.group(1))) if m else (0, 0)
print(max(devs, key=rank)["udid"])
')
echo "simulator $udid, app $bundle"
xcrun simctl boot "$udid" || true
xcrun simctl bootstatus "$udid" -b
xcrun simctl status_bar "$udid" override --time 9:41 --batteryState charged --batteryLevel 100 || true
xcrun simctl install "$udid" "$app"

# name|launch arguments. A new screen's mode is one more line here.
modes=(
  "tokens|-kithTokenGallery"
  "components|-kithComponentGallery"
  "components-bar|-kithComponentGallery -kithGallerySection bar"
  "components-avatars|-kithComponentGallery -kithGallerySection avatars"
  "components-rows|-kithComponentGallery -kithGallerySection rows"
  "components-thread|-kithComponentGallery -kithGallerySection thread"
  "components-cards|-kithComponentGallery -kithGallerySection cards"
  "components-dock|-kithComponentGallery -kithGallerySection dock"
  "demo-crew|-kithDemoScreens crew"
  "demo-crew-typing|-kithDemoScreens crew-typing"
  "demo-board|-kithDemoScreens board"
  "demo-chat|-kithDemoScreens chat"
  "demo-settings|-kithDemoScreens settings"
  "demo-computers|-kithDemoScreens computers"
  "demo-account|-kithDemoScreens account"
  "demo-account-devices|-kithDemoScreens account-devices"
  "demo-settings-problem|-kithDemoScreens problem"
  "demo-widgets|-kithDemoScreens widgets"
  "demo-agent|-kithDemoScreens agent"
  "demo-agent-memory|-kithDemoScreens agent-memory"
  "demo-agent-settings|-kithDemoScreens agent-settings"
  "demo-keys-access|-kithDemoScreens keys-access"
  "demo-keys-key|-kithDemoScreens keys-key"
  "demo-keys-replace|-kithDemoScreens keys-replace"
  "demo-keys-states|-kithDemoScreens keys-states"
  "demo-keys-board|-kithDemoScreens keys-board"
  "demo-keys-list|-kithDemoScreens keys-list"
  "demo-call|-kithDemoScreens call"
  "demo-incoming-call|-kithDemoScreens incoming-call"
  "pairing-welcome|-kithDemoScreens pairing-welcome"
  "pairing-scan|-kithDemoScreens pairing-scan"
  "pairing-install|-kithDemoScreens pairing-install"
  "pairing-success|-kithDemoScreens pairing-success"
  "pairing-error|-kithDemoScreens pairing-error"
)

for appearance in light dark; do
  xcrun simctl ui "$udid" appearance "$appearance"
  for mode in "${modes[@]}"; do
    name="${mode%%|*}"
    args="${mode#*|}"
    xcrun simctl terminate "$udid" "$bundle" 2>/dev/null || true
    # shellcheck disable=SC2086
    xcrun simctl launch "$udid" "$bundle" $args >/dev/null
    sleep 5
    xcrun simctl io "$udid" screenshot "$out/ios-$name-$appearance.png"
  done
done
