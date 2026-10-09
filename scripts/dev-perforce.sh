#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
umask 077
mkdir -p .local
chmod 700 .local
COMPOSE_IGNORE_ORPHANS=1 docker compose up -d --build --wait postgres perforce
docker compose exec -T perforce sh < scripts/seed-perforce.sh
docker compose exec -T perforce cat /data/.p4forge-credentials/build-password > .local/p4-build-password
p4_endpoint=${P4PORT:-$(docker compose port perforce 1666)}
P4TICKETS="$PWD/.local/p4tickets" p4 -p "$p4_endpoint" -u p4forge login -a < .local/p4-build-password
chmod 600 .local/p4tickets
printf 'Perforce ready. Ticket: .local/p4tickets; Stream: //streams/main\n'
