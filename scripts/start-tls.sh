#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
python3 scripts/check-cloudflare.py
docker compose -f compose.yaml -f compose.server.yaml --profile tls up -d --build --wait
printf 'Verify certificate: curl --fail https://p4forge.zenogrid.co.kr:8444/readyz\n'
