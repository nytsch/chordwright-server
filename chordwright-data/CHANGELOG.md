# Changelog

## 2.0.0

- Der Server ist neu geschrieben, in Rust statt Node: eine einzige Programmdatei von etwa 3 MB. Das Image des Add-ons schrumpft von rund 150 MB auf unter 5 MB und startet schneller; es kommt ohne Docker Hub aus (Home Assistants Basis-Image von ghcr.io zum Laden, das Image selbst ist nur die Programmdatei). Ordner, Token, Zertifikate und die einmal installierte CA bleiben, wie sie sind — für die App ändert sich nichts.
- Das Add-on baut nichts mehr auf dem Home-Assistant-Rechner: Es holt die fertige Programmdatei aus dem Release dieser Version und prüft sie gegen deren Prüfsumme.
- Derselbe Server läuft jetzt auch in der Chordwright-Desktop-App selbst (Bibliothek als Ordner auf dem Rechner); es gibt ihn nur noch einmal.
- Das Add-on hat eine Web-UI (*Web-UI öffnen*, auf Wunsch in der Seitenleiste): ein QR-Code, mit dem sich ein Gerät verbindet — mit der Kamera-App gescannt öffnet er Chordwright mit Adresse und Token, und die App kann ihn auch selbst scannen. Bei der eigenen Zertifizierungsstelle steht davor ein zweiter Code, der das Zertifikat lädt. Hat der Rechner mehrere Adressen, wählt man, für welche der Code gilt.
- Ist der Port unter *Netzwerk* umgestellt, nennen Protokoll und QR-Code jetzt diesen statt 4174.
- Mit eigenen Zertifikaten (Let's Encrypt, DuckDNS) zeigt das Protokoll die Namen aus dem Zertifikat als Adresse, ohne https die IP-Adressen des Rechners.
- Beim Beenden schreibt der Server das Änderungsprotokoll noch fertig.
- Von Hand: `chordwright-server` aus dem Release oder `cargo run --release -- --dir ./data`. Die Programmdateien für Linux sind jetzt statisch gebaut (`…-unknown-linux-musl`, auch für ARM).

## 1.7.0

- Der Server kann sich selbst eine kleine Zertifizierungsstelle anlegen (`--own-ca <ordner>`), wie es das Add-on tut, und spricht damit https unter den Namen und Adressen des Rechners. Für die Desktop-App, die eine Bibliothek im WLAN freigibt.
- `--loopback-port`: derselbe Server zusätzlich per http auf `127.0.0.1`, für das Programm, das ihn gestartet hat. `--ready-json` nennt dann beides: diese Adresse und die fürs Netz.
- Ein belegter Port oder ein Ordner, in den der Server nicht schreiben darf, beendet ihn jetzt mit einer Meldung statt ihn halb gestartet stehen zu lassen.
- Am Add-on ändert sich nichts; es nutzt dieselbe CA-Logik wie bisher, nur an anderer Stelle im Code.

## 1.6.0

- Den Server gibt es jetzt auch als einzelne Programmdatei für macOS (Intel, Apple Silicon und beides in einem), Windows und Linux, ohne installiertes Node. Sie hängt an jedem Release und ist für die Desktop-App gedacht, die damit eine Bibliothek als Ordner auf demselben Rechner führen kann.
- Für Programme, die den Server selbst starten: `--port 0` nimmt einen freien Port, `--ready-json` meldet ihn als eine JSON-Zeile, und mit `--exit-with-stdin` endet der Server, sobald das startende Programm weg ist.
- Unter Windows scheiterte ein Speichern gelegentlich, wenn die Datei im selben Moment gelesen wurde (`EPERM`). Der Server versucht es jetzt kurz erneut.
- Am Add-on ändert sich nichts.

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
