#!/bin/sh
# Startet das Add-on-Image wie Home Assistant (ohne Optionen: Token und eigene
# CA von selbst) und fragt den Server über https — mit nichts als der CA.
set -eu
image="$1"
dirs=$(mktemp -d)
mkdir -p "$dirs/data" "$dirs/share" "$dirs/ssl"
docker run -d --name addon-check --network host -e ADDON_PORT=4174 \
  -v "$dirs/data:/data" -v "$dirs/share:/share" -v "$dirs/ssl:/ssl" "$image" > /dev/null
trap 'docker rm -f addon-check > /dev/null' EXIT
for i in $(seq 50); do [ -s "$dirs/data/ca-cert.pem" ] && curl -sf --cacert "$dirs/data/ca-cert.pem" https://localhost:4174/api/health > /dev/null && break; sleep 0.2; done
docker logs addon-check
token=$(sudo cat "$dirs/data/token")
curl -sf --cacert "$dirs/data/ca-cert.pem" -H "authorization: Bearer $token" https://localhost:4174/api/library > /dev/null
docker stop addon-check > /dev/null
[ "$(docker inspect addon-check --format '{{.State.ExitCode}}')" = 0 ]
echo "Add-on-Image läuft und endet sauber."
