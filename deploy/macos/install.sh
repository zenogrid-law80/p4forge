#!/bin/sh
# Fresh installation only; never changes an existing account or installation.
set -eu
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
[ "$(id -u)" = 0 ] || { echo 'Run this installer as administrator.' >&2; exit 1; }
[ "$#" = 4 ] || { echo 'Usage: install.sh TOKEN_FILE TICKETS_FILE P4_BINARY RUST_TOOLCHAIN' >&2; exit 1; }
bundle=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=/Library/P4Forge
plist=/Library/LaunchDaemons/kr.co.zenogrid.p4forge.agent.plist
for path in "$root" "$plist"; do
  [ ! -e "$path" ] && [ ! -L "$path" ] || { echo "Already exists: $path" >&2; exit 1; }
done
if dscl . -read /Users/_p4forge >/dev/null 2>&1 || dscl . -read /Groups/_p4forge >/dev/null 2>&1; then
  echo 'The _p4forge account/group already exists; inspect it before reinstalling.' >&2; exit 1
fi
[ -z "$(dscl . -search /Users UniqueID 10003)" ] || { echo 'UID 10003 in use'; exit 1; }
[ -z "$(dscl . -search /Groups PrimaryGroupID 10003)" ] || { echo 'GID 10003 in use'; exit 1; }
for path in "$1" "$2" "$3" "$4/bin/rustc" "$4/bin/cargo"; do
  [ -f "$path" ] || { echo "Missing input: $path" >&2; exit 1; }
done
[ "$(tr -d '\r\n' < "$1" | wc -c | tr -d ' ')" = 43 ] || { echo 'Invalid token file'; exit 1; }
(cd "$bundle" && shasum -a 256 -c SHA256SUMS)
"$3" -V >/dev/null
"$4/bin/rustc" -V

install -d -o root -g wheel -m 755 "$root" "$root/bin" "$root/work" "$root/toolchain"
install -d -o root -g wheel -m 700 "$root/secrets" "$root/state" "$root/logs"
install -o root -g wheel -m 755 "$bundle/p4forge-agent" "$root/bin/p4forge-agent"
install -o root -g wheel -m 755 "$3" "$root/bin/p4"
install -o root -g wheel -m 600 "$1" "$root/secrets/agent-token"
install -o root -g wheel -m 600 "$2" "$root/secrets/p4tickets"
# A private, root-owned copy; the build account never accesses the human HOME.
ditto "$4/bin" "$root/toolchain/bin"
ditto "$4/lib" "$root/toolchain/lib"
chown -R root:wheel "$root/toolchain"
chmod -R go-w "$root/toolchain"

dscl . -create /Groups/_p4forge
dscl . -create /Groups/_p4forge PrimaryGroupID 10003
dscl . -create /Groups/_p4forge Password '*'
dscl . -create /Users/_p4forge
dscl . -create /Users/_p4forge UniqueID 10003
dscl . -create /Users/_p4forge PrimaryGroupID 10003
dscl . -create /Users/_p4forge RealName 'P4Forge Build Agent'
dscl . -create /Users/_p4forge UserShell /usr/bin/false
dscl . -create /Users/_p4forge NFSHomeDirectory /var/empty
dscl . -create /Users/_p4forge Password '*'
dscl . -create /Users/_p4forge IsHidden 1
install -o root -g wheel -m 644 "$bundle/kr.co.zenogrid.p4forge.agent.plist" "$plist"
plutil -lint "$plist"
/usr/bin/python3 "$bundle/test-process-cleanup.py"
sh "$bundle/install-self-patch.sh"
launchctl bootstrap system "$plist"
launchctl print system/kr.co.zenogrid.p4forge.agent
echo 'P4Forge installed. Logs: /Library/P4Forge/logs/agent.log'
