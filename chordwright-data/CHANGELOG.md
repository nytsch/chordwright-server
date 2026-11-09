# Changelog

## 2.7.0

- Änderungen vorschlagen: Ein Musiker kann neu **Änderungen vorschlagen** dürfen (in der Web-UI unter **Band** einstellbar). Er liest wie bisher alles, schreibt aber nichts direkt. Was er in seiner Kopie der Bibliothek ändert, schickt er als Vorschlag zurück, und erst wenn du ihn annimmst, steht es in der Bibliothek. Jeder Musiker, der mehr als nur lesen darf, kann so auch vorschlagen, was er selbst nicht darf, zum Beispiel einen Song löschen.
- Beim Annehmen wird jede Änderung auf den Stand geschrieben, auf dem sie gemacht wurde. Hat sich ein Song inzwischen geändert, bleibt er im Vorschlag offen und kommt mit dem jetzigen Stand zurück, zum Zusammenführen oder trotzdem Übernehmen. Unter **Letzte Änderungen** steht sie unter dem Namen dessen, der sie vorgeschlagen hat, und sagt wie jede andere, was sich geändert hat.
- Für die App: `GET`/`POST /api/proposals`, `GET`/`DELETE /api/proposals/:id`, `POST /api/proposals/:id/accept`; `/api/health` sagt `proposals: true`. Die Vorschläge liegen in `.chordwright/proposals/` und gehen in keine Sicherung.
- Web-UI, neuer Abschnitt **Vorschläge**: jeder offene Vorschlag mit Absender, Notiz und für jede Änderung einem Zeilenvergleich (rot was wegfällt, grün was dazukommt). **Ausgewählte übernehmen** schreibt die angehakten Änderungen, **Trotzdem übernehmen** auch die, die sich seitdem geändert haben, **Ablehnen** schließt ihn. Darunter die zuletzt entschiedenen. Die Karte der Bibliothek zeigt, wie viele Vorschläge warten.

## 2.6.2

- **Letzte Änderungen** bei Abläufen: Teile heißen nach ihrem Namen („Verse 1“) statt nach ihrer Kennung, ein neu erzeugter Teil mit gleichem Namen gilt als geändert statt als weg und neu, und geänderte Takte oder Wiederholungen stehen da — „Lord I Need You › Standard › Verse 1: Takte 8 → 12“. Ändert sich die Länge eines Ablaufs, steht auch die Summe der Takte da.
- Was in einem Set, einem Ablauf oder einem Teil geschah, nennt den ganzen Weg dorthin: Song, Ablauf, Teil.

## 2.6.1

- **Letzte Änderungen**: Ein neu angelegter Eintrag sagt jetzt auch, womit er beginnt — das erste Set mit Namen, die ersten Tags, die Tags eines Songs in den Song-Einstellungen — statt nur „neu“.

## 2.6.0

- **Letzte Änderungen** sagen jetzt, was sich geändert hat: bei Tags welcher dazukam, wegfiel oder umbenannt wurde, bei Sets welches Set und welcher Song darin, bei Songs die geänderten Angaben (z. B. Tonart G → A) und wie viele Zeilen Text. Die App zeigt das rechts neben dem Eintrag. Mehrere Speichervorgänge kurz hintereinander werden zusammen beschrieben.
- Ein Speichervorgang, der nichts ändert, steht nicht mehr in der Liste. Bisher schrieb die App die Tag-Liste bei jeder Song-Änderung neu, und jeder davon erschien als „Tags geändert“.

## 2.5.2

- Web-UI, **Band**: Die Liste der Musiker und der gewählte Musiker stehen nebeneinander, am Handy untereinander. Eine lange Liste scrollt für sich, ab neun Musikern mit einem Suchfeld; der gewählte bleibt darin im Bild, und sein QR-Code steht beim Durchklicken immer an derselben Stelle.
- Den QR-Code eines Musikers weitergeben: **Bild kopieren** legt ihn mit dem Namen darunter als Bild in die Zwischenablage, zum Einfügen in eine Nachricht. **Link kopieren** kopiert den Link dahinter — wer ihn am Handy antippt, ist gleich verbunden. **Speichern** lädt ihn als PNG herunter, am Handy teilt **Teilen** ihn direkt. Auch Rechtsklick oder langes Tippen auf den Code kopiert ihn jetzt als Bild. Der QR-Code der Bibliothek selbst bekommt diese Knöpfe nicht: Ihr Token darf alles.
- Mit eigener CA und ohne Tunnel bekommt jetzt auch jeder Musiker zwei QR-Codes wie die Bibliothek: **1 Zertifikat** (einmal pro Gerät), dann **2 Verbinden**, das Chordwright gleich mit seinem Zugang öffnet. Bisher fehlte ihm das Zertifikat, und sein Gerät ließ die App nicht an den Server. Beide lassen sich kopieren und verschicken.

## 2.5.1

- Web-UI, Abschnitt **Band**: Die Musiker stehen jetzt nach ihren Rechten sortiert — wer Songs und Sets bearbeiten darf oben, dann wer Sets bearbeiten darf, dann wer nur liest — und innerhalb davon alphabetisch nach Namen.

## 2.5.0

- Nutzung: In der Web-UI bekommt jede Bibliothek einen Abschnitt **Nutzung** — wer wann auf die Bibliothek zugegriffen hat, für die letzten 24 Stunden, 7 oder 30 Tage. Zahlen zu Anfragen, Personen, Rechenzeit und übertragenen Daten, dazu die Last des Add-ons (CPU im Schnitt, Arbeitsspeicher) und des ganzen Rechners. Zwei Ringdiagramme zeigen, wer wie viel angefragt hat und wofür (Lesen, Schreiben, Live-Verbindung, Bühne, KI, Sicherungen, Abgewiesen), Säulen den Verlauf, eine Tabelle jede Person mit ihrem letzten Zugriff, und darunter die einzelnen Besuche mit Uhrzeit.
- Musiker stehen dort mit ihrem Namen, Geräte mit dem Token der Bibliothek unter dem Namen, den die App beim Server einträgt. Gezählt wird nur, nichts vom Inhalt; die Zahlen liegen in `.chordwright/usage.json` im Ordner und reichen 31 Tage zurück. Kein Weg der API gibt sie heraus.

## 2.4.0

- KI im Ablaufplaner über den Server: In der Web-UI bekommt jede Bibliothek einen Abschnitt **KI**. Dort trägst du einen Anthropic-Schlüssel ein und stellst ein, wie viele Fragen am Tag jeder Musiker stellen darf (Voreinstellung 2) und wie viele du selbst stellst (leer: ohne Grenze). Je Musiker lässt sich das unter **Band** ändern, 0 heißt keine KI. Der Schlüssel bleibt auf dem Server und geht nie an die App und nie in eine Sicherung. Der Server legt Modell und Umfang jeder Anfrage selbst fest. Die Web-UI zeigt je Zugang die Fragen von heute und diesem Monat samt Tokens und Suchen (`.chordwright/assistant.jsonl` im Ordner).
- `POST /api/assistant` für die App. `/api/health` sagt mit `assistant`, ob ein Schlüssel hinterlegt ist, und mit einem Token in `assistantQuota`, wie viel davon heute schon verbraucht ist.

## 2.3.1

- Web-UI unter Home Assistant: Nach **Aufnehmen**, **Speichern**, **Verlängern** und beim Tipp auf einen Musiker blieb die Seite schwarz, vor allem in der App am Handy. Der Sprung zum Abschnitt Band (`#band`) scrollte auch die Seite von Home Assistant um den Iframe herum mit. Jetzt scrollt nur noch die Web-UI selbst.

## 2.3.0

- Was ein Musiker darf, stellst du je Musiker ein: **Nur lesen** (wie bisher), **Sets bearbeiten** (Sets und Besetzung) oder **Songs und Sets bearbeiten** (auch Songs und Abläufe). Songs löschen und Sicherungen bleiben bei dir. `/api/health` und `/api/band` nennen es als `can`.

## 2.2.1

- `public_url` nimmt auch einen bloßen Namen (`band.example.org`, wie bei Cloudflare eingetragen) und setzt `https://` davor; bisher galt er stillschweigend nicht, und die QR-Codes der Musiker zeigten nur Adressen im Heimnetz.
- Mit `public_url` bekommt auch der eigene QR-Code einer Bibliothek die Adresse von draußen zur Wahl (letzter Chip über dem Code, ohne Zertifikatsschritt).

## 2.2.0

- Die Band: In der Web-UI bekommt jede Bibliothek eine Liste ihrer Musiker, jeder mit Rollen (D, P, A, E, B, S) und eigenem Zugang samt QR-Code, befristet (1 Tag bis 30 Tage) oder bis auf Widerruf. Ein Musiker liest Songs, Sets und Abläufe, spielt auf der gemeinsamen Bühne mit und trägt ein, wann er kann — sonst schreibt er nichts. Verlängern behält den Zugang, Von der Liste nehmen sperrt ihn sofort.
- Von unterwegs über einen Tunnel: Mit `tunnel` hört der Server zusätzlich ohne https auf Port 4175, etwa für das Cloudflared-Add-on; `public_url` ist die Adresse von draußen und steht in den QR-Codes der Musiker. Anleitung in der Dokumentation.
- `/api/health` sagt mit einem Token, was er darf (`access`: `owner`, `member`, `expired`, `none`; beim Musiker `member` und `until`), `/api/band` nennt die Musiker mit ihren Rollen.

## 2.1.0

- Mehrere Bibliotheken: In der Web-UI legst du weitere an, jede mit eigenem Ordner (`share/chordwright-<name>`), eigenem Token, eigenen Sicherungen und eigener Bühne, erreichbar unter derselben Adresse mit `/i/<name>`. Umbenennen, neuer Token und Entfernen (der Ordner bleibt) gehen dort auch. Für die App ändert sich nichts: Sie verbindet sich per QR-Code wie bisher.
- Die Web-UI zeigt jetzt mehr als den QR-Code: jede Bibliothek mit Songs, Sets, gerade verbundenen Geräten und letzter Änderung; zur gewählten Größe, Sicherungen, die letzten Änderungen mit Namen und offene Bühnen.
- Das Add-on trägt das Chordwright-Zeichen als Icon und Logo im Add-on-Store.

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
