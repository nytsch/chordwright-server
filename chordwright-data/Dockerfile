# Das Add-on ist eine einzige Programmdatei: der Server, statisch gebaut (musl),
# aus dem Release, das zur Version des Add-ons gehört (config.yaml), geprüft
# gegen dessen SHA256SUMS. Home Assistant reicht BUILD_ARCH und BUILD_VERSION
# herein; gebaut wird hier nichts — Rust auf einem Raspberry Pi zu übersetzen
# dauerte länger als jedes Update.
#
# Deshalb erst das Release (Tag → release.yml hängt die Dateien an), dann die
# Version in config.yaml auf main heben: sonst findet Home Assistant die Datei
# noch nicht.
#
# Nichts von Docker Hub: Geladen wird mit Home Assistants eigenem Basis-Image
# (ghcr.io, je Architektur aus build.yaml, BUILD_FROM), und das Image selbst ist
# nur die statische Programmdatei — sie braucht kein Betriebssystem darunter.
ARG BUILD_FROM=ghcr.io/home-assistant/amd64-base:3.20
FROM ${BUILD_FROM} AS fetch
ARG BUILD_ARCH=amd64
ARG BUILD_VERSION
ARG RELEASES=https://github.com/nytsch/chordwright-server/releases/download
RUN set -eu; \
    [ -n "$BUILD_VERSION" ] || { echo "BUILD_VERSION fehlt" >&2; exit 1; }; \
    case "$BUILD_ARCH" in \
      aarch64) target=aarch64-unknown-linux-musl ;; \
      amd64) target=x86_64-unknown-linux-musl ;; \
      *) echo "Keine Programmdatei für $BUILD_ARCH" >&2; exit 1 ;; \
    esac; \
    archive="chordwright-server-$target.tar.gz"; \
    wget -q -O "$archive" "$RELEASES/v$BUILD_VERSION/$archive"; \
    wget -q -O SHA256SUMS "$RELEASES/v$BUILD_VERSION/SHA256SUMS"; \
    grep " $archive\$" SHA256SUMS | sha256sum -c -; \
    tar -xzf "$archive"; \
    mv "chordwright-server-$target" /chordwright-server

FROM scratch
COPY --from=fetch /chordwright-server /chordwright-server
CMD ["/chordwright-server", "--addon"]
