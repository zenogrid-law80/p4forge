#!/bin/sh
set -eu
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
[ "$(id -u)" = 0 ] && [ "$#" = 2 ] || { echo 'Usage (administrator): enable-publisher.sh POLICY_JSON TICKETS'; exit 1; }
root=/Library/P4Forge
[ -d "$root/secrets" ] && [ ! -L "$root/secrets" ] && [ "$(stat -f %u "$root/secrets")" = 0 ] || exit 1
[ -f "$1" ] && [ ! -L "$1" ] && [ -f "$2" ] && [ ! -L "$2" ] || exit 1
install -o root -g wheel -m 600 "$1" "$root/secrets/publisher.json"
install -o root -g wheel -m 600 "$2" "$root/secrets/publisher-tickets"
echo 'Publisher policy installed. Run update.sh to activate with the new Agent.'
