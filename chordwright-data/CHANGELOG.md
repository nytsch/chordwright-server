# Changelog

## 1.2.0

- Einstellungen, Setlists, Tags und Bibliothek werden zwischen Geräten zusammengeführt statt überschrieben.
- Ein einzelnes Lesen (auch ein 404) meldet, dass der Server Revisionen kennt.

## 1.1.0

- Revisionen für jeden Datensatz; bedingtes Schreiben (`If-Match`, `If-None-Match: *`), 412 bei Konflikt.

## 1.0.0

- Erste Version: Bibliothek unter `/share/chordwright`, Token, https mit Zertifikaten aus `/ssl` oder selbstsigniert.
