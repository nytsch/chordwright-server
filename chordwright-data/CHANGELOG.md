# Changelog

## 1.2.0

- Sicherungen: automatisch nach Zeitplan (neue Optionen `backup_every_hours`, `backup_keep`), nur wenn sich etwas geändert hat, und auf Knopfdruck aus der App. Sie liegen als Ordner unter `share/chordwright/backups/`.
- Wiederherstellen aus der App; der Stand davor wird vorher selbst gesichert.
- Neue Schnittstelle `/api/backups`.

## 1.1.0

- Eigene kleine Zertifizierungsstelle statt eines selbstsignierten Zertifikats: einmal pro Gerät installieren (`/ca.crt`), danach keine Warnungen mehr — auch nicht in einer App vom Home-Bildschirm.
- Das Zertifikat trägt die IP-Adressen des Home-Assistant-Rechners (per Supervisor) und die Namen aus der neuen Option `hostnames`; ändert sich eine Adresse, wird es neu ausgestellt.
- Das Protokoll zeigt die tatsächlichen Adressen.

## 1.0.0

Erste Version.

- Bibliothek als Ordner mit `.chordpro`-Dateien unter `/share/chordwright`, per Samba erreichbar.
- Token wird beim ersten Start erzeugt; https mit den Zertifikaten aus `/ssl` oder selbstsigniert.
- Revisionen für jeden Datensatz: Änderungen von zwei Geräten werden erkannt und zusammengeführt statt überschrieben.
