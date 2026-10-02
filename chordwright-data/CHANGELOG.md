# Changelog

## 1.5.0

- Gemeinsame Bühne: Das leitende Gerät sieht, wer gerade folgt — mit Namen, und ob jemand gerade mitläuft oder sich abgekoppelt hat. Neue Schnittstelle `/api/stage/:raum/here`; der Bühnenstand nennt die Folgenden unter `followers`.

## 1.4.0

- Gemeinsame Bühne: Ein Gerät leitet, die anderen folgen — Set, Lied, Liedwechsel und Start, im Takt. Mehrere Bühnen gleichzeitig, jede mit eigenem Namen. Neue Schnittstellen `/api/stage` und `/api/time`; die Bühnenstände gehen als benanntes Ereignis `stage` über `/api/events`.

## 1.3.1

- Ein Datensatz, den es noch nicht gibt, kann mit `?quiet=1` als `200` statt `404` erfragt werden. Die App tut das und füllt damit nicht mehr bei jedem Start die Browser-Konsole mit roten 404-Zeilen.
- Behoben: Etwa jedes zweihundertste Zertifikat der eigenen CA war ungültig (Seriennummer mit führender Null, von OpenSSL 3 als „illegal padding“ abgelehnt). Das Add-on konnte dann nicht starten; ein Neustart half meist.

## 1.3.0

- Der Server merkt sich, wer welchen Datensatz zuletzt wann geändert hat (Name aus der App, `X-Chordwright-User`), auch Änderungen von Hand per Samba und Wiederherstellungen. Neue Schnittstelle `/api/changes`; Lesezugriffe liefern die letzte Änderung mit.

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
