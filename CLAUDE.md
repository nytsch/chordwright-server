# Hinweise für KI-Assistenten

## Release: erst Tag, dann main

Home Assistant bietet das Add-on-Update an, sobald `version:` in
`chordwright-data/config.yaml` auf `main` steigt, und das Image lädt das
Programm aus dem GitHub-Release dieser Version. Gibt es das Release noch
nicht, schlägt das Update fehl. Deshalb immer in dieser Reihenfolge:

1. Auf dem Feature-Branch die Version überall gleich erhöhen: `Cargo.toml`
   (danach `cargo build`, damit `Cargo.lock` mitzieht),
   `chordwright-data/config.yaml`, `package.json`, `compose.yaml`
   (`BUILD_VERSION`), dazu einen Abschnitt in `chordwright-data/CHANGELOG.md`.
   `release.yml` bricht ab, wenn Tag und Dateien nicht dieselbe Version nennen.
2. Den Tag `vX.Y.Z` auf den letzten Commit des **Branches** setzen und pushen,
   **vor** dem Merge:
   `git fetch origin <branch> && git tag vX.Y.Z FETCH_HEAD && git push origin vX.Y.Z`
3. Erst wenn der Release-Lauf grün ist, den PR mergen, als normalen Merge
   (kein Squash), damit der getaggte Commit in `main` liegt.

Nie zuerst mergen und danach auf `main` taggen.
