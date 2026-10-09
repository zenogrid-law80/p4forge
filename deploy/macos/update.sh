#!/bin/sh
set -eu
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
[ "$(id -u)" = 0 ] || { echo 'Run as administrator.' >&2; exit 1; }
bundle=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=/Library/P4Forge
plist=/Library/LaunchDaemons/kr.co.zenogrid.p4forge.agent.plist
[ -d "$root/state" ] && [ -f "$plist" ] || { echo 'Use install.sh for a fresh installation'; exit 1; }
[ ! -e "$root/state/self-patch/pending.json" ] && [ ! -e "$root/state/self-patch/journal.json" ] || { echo 'Finish the pending self patch first.'; exit 1; }
[ "$(stat -f %u "$root")" = 0 ] && [ ! -L "$root" ] || exit 1
[ "$(id -u _p4forge)" = 10003 ] && [ "$(id -g _p4forge)" = 10003 ] || exit 1
(cd "$bundle" && shasum -a 256 -c SHA256SUMS)
# Drain active work through P4Forge before an operator invokes an update.
if launchctl print system/kr.co.zenogrid.p4forge.agent >/dev/null 2>&1; then
  launchctl bootout system/kr.co.zenogrid.p4forge.agent
fi
# bootout can return before launchd has removed the old service entry.
remaining=20
while launchctl print system/kr.co.zenogrid.p4forge.agent >/dev/null 2>&1; do
  [ "$remaining" -gt 0 ] || { echo 'Agent did not stop within 20 seconds'; exit 1; }
  sleep 1
  remaining=$((remaining - 1))
done
install -o root -g wheel -m 755 "$bundle/p4forge-agent" "$root/bin/p4forge-agent.new"
codesign --verify --strict "$root/bin/p4forge-agent.new"
mv -f "$root/bin/p4forge-agent.new" "$root/bin/p4forge-agent"
install -o root -g wheel -m 644 "$bundle/kr.co.zenogrid.p4forge.agent.plist" "$plist"
if [ -f "$root/secrets/publisher.json" ]; then
  /usr/libexec/PlistBuddy -c "Add :EnvironmentVariables:P4FORGE_PUBLISHER_POLICY string $root/secrets/publisher.json" "$plist"
fi
/usr/bin/python3 "$bundle/test-process-cleanup.py"
sh "$bundle/install-self-patch.sh"
launchctl bootstrap system "$plist"
launchctl print system/kr.co.zenogrid.p4forge.agent
echo 'Updated and started P4Forge Agent.'
