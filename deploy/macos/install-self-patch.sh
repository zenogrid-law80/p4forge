#!/bin/sh
set -eu
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
[ "$(id -u)" = 0 ] || { echo 'Run as administrator.' >&2; exit 1; }
bundle=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=/Library/P4Forge
state="$root/state/self-patch"
plist=/Library/LaunchDaemons/kr.co.zenogrid.p4forge.updater.plist
[ -d "$root/state" ] && [ ! -L "$root/state" ] && [ "$(stat -f %u "$root/state")" = 0 ] || exit 1
[ ! -e "$state/pending.json" ] && [ ! -e "$state/journal.json" ] || { echo 'Finish the pending self patch first.' >&2; exit 1; }
(cd "$bundle" && shasum -a 256 -c SHA256SUMS)
if launchctl print system/kr.co.zenogrid.p4forge.updater >/dev/null 2>&1; then
  launchctl bootout system/kr.co.zenogrid.p4forge.updater
fi
remaining=20
while launchctl print system/kr.co.zenogrid.p4forge.updater >/dev/null 2>&1; do
  [ "$remaining" -gt 0 ] || exit 1
  sleep 1
  remaining=$((remaining - 1))
done
install -d -o root -g wheel -m 700 "$state"
install -o root -g wheel -m 755 "$bundle/p4forge-agent" "$root/bin/p4forge-updater"
codesign --verify --strict "$root/bin/p4forge-updater"
install -o root -g wheel -m 644 "$bundle/kr.co.zenogrid.p4forge.updater.plist" "$plist"
plutil -lint "$plist"
launchctl bootstrap system "$plist"
echo 'Self-patch worker installed.'
