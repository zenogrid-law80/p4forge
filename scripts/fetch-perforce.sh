#!/bin/sh
set -eu
destination=${1:-/usr/local/bin}
architecture=${TARGETARCH:-$(uname -m)}
case "$architecture" in
  arm64|aarch64)
    platform=bin.linux26aarch64
    p4_sum=485d6348c7192bea3536a1356884397f930f91704351095c9ce7210f8660f9a8
    p4d_sum=8c5c47f6bba6827cf567598ad8e0e25ba434558384d28a81fa833ed6f4e12a3c
    ;;
  amd64|x86_64)
    platform=bin.linux26x86_64
    p4_sum=f03c7c33c04fac1c6941a97b6bb4be649c54a416fd085a80dd70fb79b1555383
    p4d_sum=a087490c5d226dac25ed6040012bea6b0eb522198d9aa0486c03efd9bd9d56f3
    ;;
  *) printf 'Unsupported Perforce architecture\n' >&2; exit 1 ;;
esac
mkdir -p "$destination"
for binary in p4 p4d; do
  curl --fail --silent --show-error --location "https://ftp.perforce.com/perforce/r26.1/$platform/$binary" --output "$destination/$binary"
  case "$binary" in p4) checksum=$p4_sum ;; p4d) checksum=$p4d_sum ;; esac
  printf '%s  %s\n' "$checksum" "$destination/$binary" | sha256sum -c -
  chmod 755 "$destination/$binary"
done
