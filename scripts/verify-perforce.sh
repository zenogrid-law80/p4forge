#!/bin/sh
# Prove that a fresh read-only build client syncs the old CL after a newer submit.
set -eu
cd "$(dirname "$0")/.."
export P4PORT="${P4PORT:-$(docker compose port perforce 1666)}" P4USER=p4forge P4TICKETS="$PWD/.local/p4tickets"
export P4CLIENT="p4forge_smoke_$$"
directory=$(mktemp -d "$PWD/.local/source-check.XXXXXX")
cleanup() { p4 client -d "$P4CLIENT" >/dev/null 2>&1 || :; rm -rf "$directory"; }
trap cleanup EXIT HUP INT TERM
p4 client -i <<EOF
Client: $P4CLIENT
Owner: p4forge
Root: $directory
Stream: //streams/main
StreamAtChange: 2
LineEnd: unix
EOF
p4 sync -q "//$P4CLIENT/...@2"
test "$(cat "$directory/hello.txt")" = 'first revision'
printf 'Old CL 2 checkout verified after CL 3; temporary client cleaned up on exit.\n'
