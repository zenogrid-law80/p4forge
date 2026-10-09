#!/bin/sh
set -eu
# Run only inside the dedicated development Perforce container.
export P4PORT=localhost:1666 P4USER=p4forge-admin
umask 077
mkdir -p /data/.p4forge-credentials
admin_password=/data/.p4forge-credentials/admin-password
if [ ! -f "$admin_password" ]; then
  if [ -f /home/perforce/.admin-password ]; then
    mv /home/perforce/.admin-password "$admin_password"
  else
    { printf 'P4forge!'; od -An -N24 -tx1 /dev/urandom | tr -d ' \n'; printf '\n'; } > "$admin_password"
    password=$(cat "$admin_password")
    printf '%s\n%s\n' "$password" "$password" | p4 passwd
  fi
fi
printf '%s\n' "$(cat "$admin_password")" | p4 login
[ -f /data/server.id ] || p4 serverid p4forge-dev
p4 user -i <<'EOF'
User: p4forge-admin
Email: p4forge-admin@example.invalid
FullName: P4Forge Development Administrator
EOF
p4 user -f -i <<'EOF'
User: p4forge
Email: p4forge@example.invalid
FullName: P4Forge Build User
EOF
build_password=/data/.p4forge-credentials/build-password
if [ ! -f "$build_password" ]; then
  { printf 'P4forge!'; od -An -N24 -tx1 /dev/urandom | tr -d ' \n'; printf '\n'; } > "$build_password"
  password=$(cat "$build_password")
  printf '%s\n%s\n' "$password" "$password" | p4 passwd p4forge
fi
if [ ! -f /data/.p4forge-credentials/build-ready ]; then
  { printf 'P4forge!'; od -An -N24 -tx1 /dev/urandom | tr -d ' \n'; printf '\n'; } > "$build_password.next"
  old_password=$(cat "$build_password")
  password=$(cat "$build_password.next")
  printf '%s\n%s\n%s\n' "$old_password" "$password" "$password" | p4 -u p4forge passwd
  mv "$build_password.next" "$build_password"
  touch /data/.p4forge-credentials/build-ready
fi
# Preserve publisher and operator rules when refreshing the demo server.
protection_file=$(mktemp)
p4 protect -o > "$protection_file"
# The initial server's default wildcard write rule is not a CI account policy.
if ! grep -q 'super user p4forge-admin ' "$protection_file"; then
  sed '/^[[:space:]]*write[[:space:]]*user[[:space:]]*\*[[:space:]]*\*[[:space:]]*\/\/\.\.\.[[:space:]]*$/d' "$protection_file" > "$protection_file.next"
  mv "$protection_file.next" "$protection_file"
fi
grep -q 'super user p4forge-admin ' "$protection_file" || printf '    super user p4forge-admin * //...\n' >> "$protection_file"
grep -q 'read user p4forge ' "$protection_file" || printf '    read user p4forge * //streams/...\n' >> "$protection_file"
p4 protect -i < "$protection_file"
rm -f "$protection_file"

if p4 -ztag files //streams/main/... 2>/dev/null | grep -q depotFile; then
  p4 changes -m 1 -s submitted //streams/main/...
  exit 0
fi
p4 depot -i <<'EOF'
Depot: streams
Type: stream
Map: streams/...
StreamDepth: //streams/1
EOF
p4 stream -i <<'EOF'
Stream: //streams/main
Owner: p4forge-admin
Name: P4Forge Main
Parent: none
Type: mainline
ParentView: inherit
Paths:
    share ...
EOF
mkdir -p /tmp/p4forge-seed
p4 client -i <<'EOF'
Client: p4forge_seed
Owner: p4forge-admin
Root: /tmp/p4forge-seed
Stream: //streams/main
Options: noallwrite noclobber nocompress unlocked nomodtime rmdir
LineEnd: unix
EOF
export P4CLIENT=p4forge_seed
cd /tmp/p4forge-seed
printf 'first revision\n' > hello.txt
p4 add hello.txt
p4 submit -d 'P4Forge demo: first revision'
p4 edit hello.txt
printf 'second revision\n' > hello.txt
p4 submit -d 'P4Forge demo: second revision'
p4 client -d p4forge_seed
p4 changes -s submitted //streams/main/...
