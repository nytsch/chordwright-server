# Chordwright Data

Deine Chordwright-Bibliothek liegt dann auf dem Home-Assistant-Rechner statt im Browser: ein Ordner mit einer `.chordpro`-Datei pro Song. Alle Geräte, die du verbindest, sehen dieselben Songs, und du kannst sie per Samba in jedem Editor öffnen.

## Installieren (einmalig, ca. 5 Minuten)

1. **Repository eintragen:** *Einstellungen → Add-ons → Add-on-Store*, oben rechts **⋮ → Repositories**, dort `https://github.com/nytsch/chordwright-server` eintragen und **Hinzufügen**.
2. **Add-on finden:** Die Seite neu laden. Unten erscheint der Abschnitt **Chordwright** mit **Chordwright Data**.
3. **Installieren** klicken. Home Assistant baut das Add-on jetzt, das dauert ein bis zwei Minuten.
4. **Samba einschalten**, falls noch nicht geschehen, damit du die Songs später als Dateien siehst: im Add-on-Store „Samba share" installieren, Benutzername und Passwort setzen, starten.
5. **Starten**, dann den Reiter **Protokoll** öffnen. Dort steht der **Token**, den du gleich brauchst:

   ```
   Chordwright Data
     Ordner   /share/chordwright  (per Samba: share/chordwright)
     Adresse  https://<deine-Home-Assistant-Adresse>:<Port>   (Standard-Port 4174)
     Token    3f9c…   (automatisch erzeugt)
   ```

## Die App verbinden

### Per QR-Code

Beim Add-on oben **Web-UI öffnen** (oder in der Seitenleiste **Chordwright**, wenn du *In Seitenleiste anzeigen* einschaltest). Dort stehen zwei QR-Codes:

1. **Zertifikat** — nur bei der eigenen Zertifizierungsstelle, einmal pro Gerät (siehe [Das Zertifikat](#das-zertifikat)). Mit der Kamera-App scannen und öffnen.
2. **Verbinden** — mit der Kamera-App scannen: Chordwright öffnet sich mit Adresse und Token schon eingetragen. Oder in Chordwright unter **Einstellungen → Datenquelle → Server hinzufügen** auf **QR-Code scannen** tippen.

Dann noch **Verbinden**. Hat der Rechner mehrere Adressen, wählst du oben auf der Seite, für welche der Code gilt. Bist du gerade auf dem Gerät, das du verbinden willst, tipp auf **hier öffnen**.

### Mehrere Bibliotheken

Eine Band, ein Chor, eine zum Ausprobieren: In der Web-UI legst du unter **Neue Bibliothek** weitere an. Jede hat eigene Songs, Sets, Sicherungen, eine eigene Bühne und einen eigenen Token und liegt in einem eigenen Ordner, z. B. `share/chordwright-band`. Die App erreicht sie unter derselben Adresse mit `/i/<name>` dahinter, etwa `https://192.168.1.20:4174/i/band` — der QR-Code der Bibliothek trägt das schon ein. Ein Gerät mit dem Token der einen Bibliothek kommt an die anderen nicht heran.

Unter **Verwalten** benennst du eine Bibliothek um, gibst ihr einen neuen Token (danach muss sich jedes Gerät neu verbinden) oder entfernst sie. Entfernen löscht nichts: Der Ordner bleibt in share, und legst du wieder eine mit demselben Namen an, ist alles wieder da. Die Hauptbibliothek stellst du weiter unter **Konfiguration** ein.

### Die Band: ein Zugang pro Musiker

Deine Musiker sollen Songs, Sets und Abläufe sehen, der Bühne folgen und eintragen, an welchem Sonntag sie können? In der Web-UI unter **Band** einen Namen eintragen, die Rollen anhaken (**D** Drum, **P** Piano, **A** A-Git, **E** E-Git, **B** Bass, **S** Singer), wählen, was er darf, eine Dauer wählen (1 Tag bis bis auf Widerruf) und **Aufnehmen**. Jeder Musiker bekommt einen eigenen Zugang mit eigenem QR-Code.

- **Lesen:** Songs, Sets, Abläufe, Einstellungen, letzte Änderungen, wer mit welchen Rollen dabei ist.
- **Bühne:** folgen und auch leiten.
- **Schreiben:** die eigenen Antworten, wann er kann (`share/chordwright/user/availability.<name>.json`), und zwar unter seinem Namen. Dazu, je nachdem, was du bei ihm einstellst:
  - **Nur lesen** (Voreinstellung): sonst nichts.
  - **Sets bearbeiten:** Sets anlegen, ändern, löschen und die Besetzung einteilen.
  - **Songs und Sets bearbeiten:** außerdem Songs anlegen und ändern, samt Abläufen, Song-Einstellungen und den übrigen Einstellungen der Bibliothek.
- **Bleibt bei dir:** Songs löschen, Sicherungen ansehen oder wiederherstellen, die Antworten anderer. Der Server weist das ab (`403 read only`), gleich was die App versucht.

Tipp auf einen Namen zeigt seinen QR-Code. Dort änderst du Name, Rollen und was er darf, **Verlängern** rückt nur das Ende (der Zugang bleibt, verbundene Geräte bleiben verbunden), **Von der Liste nehmen** sperrt ihn sofort; seine Antworten bleiben im Ordner. Ist die Zeit um, kommt niemand mehr mit diesem Zugang hinein. Dein eigener Token ändert sich dabei nie. Jede Bibliothek hat ihre eigenen Musiker.

Ohne Tunnel zeigt der QR-Code die Adresse im Heimnetz. Für unterwegs: [Von unterwegs](#von-unterwegs-tunnel-mit-cloudflared).

### Was die Web-UI zeigt

Oben jede Bibliothek mit Songs, Sets, gerade verbundenen Geräten und der letzten Änderung. Zur gewählten außerdem die Größe, die Sicherungen, die letzten Änderungen (wer was wann) und offene Bühnen samt dem, der gerade leitet.

### Von Hand

In Chordwright: **Einstellungen → Datenquelle**

- **Adresse:** eine der Adressen aus dem Protokoll, z. B. `https://192.168.1.20:4174`, ohne `/api` am Ende.
- **Token:** den aus dem Protokoll
- **Testen**, dann **Verbinden**. Die App lädt neu und liest und schreibt ab jetzt den Ordner auf dem Home-Assistant-Rechner.

Verbindet sich die erste App mit dem leeren Ordner, schreibt sie ihre Songs hinein. Mach das also von dem Gerät aus, auf dem deine Bibliothek gerade liegt. Jedes weitere Gerät verbindest du genauso.

Chrome fragt beim ersten Mal, ob die Seite auf Geräte im lokalen Netzwerk zugreifen darf. Das mit **Zulassen** bestätigen.

## Das Zertifikat

Die App läuft auf GitHub Pages, also über `https`. Browser lassen eine https-Seite nicht mit einem `http`-Server reden. Darum spricht das Add-on selbst https. Dabei gibt es zwei Fälle:

**Du hast schon Zertifikate in Home Assistant** (Add-on „Let's Encrypt" oder „Duck DNS"): Die liegen in `/ssl` als `fullchain.pem` und `privkey.pem`, und das Add-on nimmt sie automatisch. Als Adresse trägst du dann den Namen ein, für den das Zertifikat gilt, z. B. `https://deinname.duckdns.org:4174`. Heißen die Dateien anders, trag die Namen unter **Konfiguration** ein.

**Du hast keine** (der Normalfall, Home Assistant unter `http://homeassistant.local:8123`): Dann legt das Add-on eine **eigene kleine Zertifizierungsstelle** an und stellt sich damit ein Zertifikat für seine Adressen aus. Die IP-Adresse des Home-Assistant-Rechners holt es sich selbst; sie steht im Protokoll. Du installierst die Zertifizierungsstelle **einmal pro Gerät**, danach gibt es keine Warnungen mehr, auch nicht in der App auf dem Home-Bildschirm.

**iPhone / iPad**

1. In **Safari** `https://<Adresse aus dem Protokoll>:4174/ca.crt` öffnen. Beim ersten Mal kommt eine Warnung: *Details einblenden → Diese Website besuchen*.
2. „Profil geladen" bestätigen, dann *Einstellungen → Profil geladen → Installieren*.
3. *Einstellungen → Allgemein → Info → Zertifikatsvertrauenseinstellungen* → bei **Chordwright lokale CA** den Schalter einschalten. Ohne diesen Schritt geht es nicht.

**Mac**

Die Datei per Samba aus **share → chordwright → chordwright-ca.crt** holen (oder die Adresse oben im Browser laden), doppelklicken, in der Schlüsselbundverwaltung **Chordwright lokale CA** öffnen und unter *Vertrauen* „Immer vertrauen" wählen.

**Windows / Android:** Die Datei `chordwright-ca.crt` als vertrauenswürdige Stammzertifizierungsstelle bzw. als CA-Zertifikat installieren.

Die Zertifizierungsstelle bleibt dieselbe, solange du das Add-on nicht deinstallierst. Bekommt der Rechner eine neue IP-Adresse, stellt das Add-on beim nächsten Start ein neues Server-Zertifikat aus; auf den Geräten musst du nichts neu machen. Erreichst du Home Assistant unter einem weiteren Namen (z. B. `ha.fritz.box`), trag ihn unter **Konfiguration → hostnames** ein.

## Konfiguration

| Option | Standard | |
|---|---|---|
| `token` | leer | Leer lassen: Dann wird einmal einer erzeugt und gemerkt. Eigenen setzen: Dann gilt der. |
| `ssl` | an | Aus nur, wenn die App selbst über `http` läuft (z. B. `npm run dev` im LAN). |
| `certfile` / `keyfile` | `fullchain.pem` / `privkey.pem` | Dateinamen in `/ssl` |
| `folder` | `chordwright` | Unterordner in `share`, in dem die Bibliothek liegt |
| `backup_every_hours` | `24` | Alle so viele Stunden eine Sicherung, wenn sich etwas geändert hat. `0`: nur auf Knopfdruck |
| `backup_keep` | `14` | So viele automatische Sicherungen bleiben |
| `hostnames` | leer | Weitere Namen oder Adressen fürs Zertifikat, z. B. `ha.fritz.box` |
| `tunnel` | aus | Zusätzlich http ohne Zertifikat auf Port 4175, für einen Tunnel (siehe [Von unterwegs](#von-unterwegs-tunnel-mit-cloudflared)) |
| `public_url` | leer | Wo die Bibliothek über den Tunnel erreichbar ist, z. B. `https://songs.example.org`; steht in den QR-Codes der Musiker |

Den Port änderst du unter **Netzwerk**. In der App trägst du dann diesen Port ein.

## Von unterwegs: Tunnel mit Cloudflared

Damit die Band auch von zu Hause an die Bibliothek kommt, ohne Portfreigabe am Router: ein Tunnel zu Cloudflare. Du brauchst eine eigene Domain bei Cloudflare (kostenloser Tarif reicht).

1. Das Add-on **Cloudflared** installieren (Repository `https://github.com/homeassistant-apps/repository`), aber noch nicht starten.
2. Hier unter **Konfiguration**: `tunnel` einschalten und bei `public_url` die Adresse eintragen, unter der die Bibliothek draußen erreichbar sein soll, z. B. `https://songs.example.org` (ein bloßer Name wie `songs.example.org` reicht auch). Add-on neu starten; im Protokoll steht dann `Von draußen https://songs.example.org`, sonst gilt die Adresse nicht. Es hört jetzt zusätzlich ohne https auf Port 4175 — die Verschlüsselung übernimmt der Tunnel.
3. Im Cloudflared-Add-on unter **Konfiguration → additional_hosts**:

   ```yaml
   - hostname: songs.example.org
     service: http://192.168.1.20:4175
   ```

   mit der IP-Adresse deines Home Assistant (*Einstellungen → System → Netzwerk*). `external_hostname` bleibt leer, wenn Home Assistant selbst nicht ins Internet soll. Cloudflared starten; beim ersten Start steht im Protokoll ein Anmelde-Link: öffnen, bei Cloudflare anmelden, die Domain wählen. Den DNS-Eintrag legt es selbst an.
5. Prüfen: `https://songs.example.org/api/health` im Browser zeigt `{"ok":true,…}`.
6. In der Web-UI unter **Band** steht jetzt `https://songs.example.org` in den QR-Codes der Musiker. Weitere Bibliotheken liegen darunter mit `/i/<name>`.

Über den Tunnel gilt dein eigener Token genauso — du kannst dich also auch unterwegs mit vollen Rechten verbinden. Halte ihn deshalb für dich; jeder Musiker bekommt nur seinen. Beide sind lange Zufallszahlen, die sich nicht erraten lassen.

Port 4175 ist im Heimnetz ohne https erreichbar. Willst du das nicht, stell ihn unter **Netzwerk** auf eine andere Nummer und trag die bei `service` ein; ohne Tunnel lass `tunnel` aus.

## Die Songs als Dateien

Per Samba unter **share → chordwright**:

```
library/songs/*.chordpro   die Songs, in jedem Editor bearbeitbar
library/index.json         Titel, Tonart, Tempo
user/                      Capo, Abläufe, Setlists, Tags
backups/                   Sicherungen, je ein Ordner mit Datum
```

Speicherst du einen Song, sieht eine offene App die Änderung sofort. Legst du eine neue `.chordpro`-Datei ab, erscheint sie beim nächsten Start der App als Song.

## Wer hat was geändert

In der App trägst du beim Server deinen Namen ein. Der Server merkt sich dann zu jedem Song, jeder Setlist und jeder Einstellung, wer sie zuletzt wann geändert hat, und die App zeigt das unter *Datenquelle → Letzte Änderungen* und bei einem Konflikt zwischen zwei Geräten. Eine per Samba gespeicherte Datei steht dort als „von Hand“. Das Protokoll liegt in `share/chordwright/.chordwright/changes.json`.

## Sicherungen

Das Add-on sichert Bibliothek, Abläufe und Einstellungen selbst, nach `share/chordwright/backups/`:

- **Automatisch** alle `backup_every_hours` Stunden, aber nur, wenn sich seit der letzten Sicherung etwas geändert hat. Die neuesten `backup_keep` bleiben.
- **Auf Knopfdruck** in der App unter *Einstellungen → Sicherung → Auf dem Server*. Diese bleiben, bis du sie löschst.
- **Vor jedem Wiederherstellen** sichert der Server den Stand davor, damit sich auch das rückgängig machen lässt.

Jede Sicherung ist ein normaler Ordner. Einen einzelnen Song holst du per Samba zurück, indem du seine Datei aus `backups/<datum>/library/songs/` nach `library/songs/` kopierst.

Die Sicherungen liegen auf demselben Rechner wie die Bibliothek. Gegen einen Ausfall des Rechners hilft nur eine Kopie woanders hin: Die Songs und die Sicherungen landen in Home-Assistant-Sicherungen, wenn darin der Ordner „Share" mitgesichert wird.

## Gemeinsame Bühne

Ab Version 1.4.0 kann ein Gerät auf der Bühne **leiten** und die anderen **folgen**: Das leitende Gerät gibt Set, Lied, Liedwechsel und Start vor, die folgenden machen mit, im Takt. Eingestellt wird das in der App unter *Einstellungen → Bühne → Gemeinsame Bühne*. Das leitende Gerät sieht, wer gerade folgt und wer sich abgekoppelt hat. Mehrere Bühnen (etwa zwei Bands) können gleichzeitig über denselben Server laufen; jede hat einen eigenen Namen.

Am Add-on ist dafür nichts einzustellen. Es braucht keinen MQTT-Broker: Die Bühne läuft über dieselbe Verbindung wie die Bibliothek. Der Stand liegt nur im Speicher und verschwindet bei einem Neustart des Add-ons (das leitende Gerät stellt ihn nach wenigen Sekunden wieder her).

## Aktualisieren

Neue Versionen erscheinen im Add-on-Store wie bei jedem anderen Add-on; **Aktualisieren** klicken. Die Songs, der Token und das Zertifikat bleiben erhalten.

## Ohne Zugriff auf GitHub: als lokales Add-on

Falls Home Assistant das Repository nicht erreicht (etwa weil es privat ist): per Samba die Freigabe **addons** öffnen, den Ordner `chordwright-data` aus diesem Repository hineinziehen, sodass dort `addons/chordwright-data/config.yaml` liegt. Dann im Add-on-Store **⋮ → Nach Updates suchen**; das Add-on erscheint unter **Lokale Add-ons**. Für ein Update den Ordner erneut kopieren.

## Wenn etwas nicht geht

- **„Testen" schlägt fehl:** Läuft das Add-on (Protokoll)? Ist die Zertifizierungsstelle auf diesem Gerät installiert und (iPhone) in den Zertifikatsvertrauenseinstellungen eingeschaltet? Steht die Adresse im Protokoll unter „Gilt für"? Steht in der Adresse `https`?
- **Der Token wird abgelehnt:** Den Token noch einmal aus dem Protokoll kopieren, ohne Leerzeichen.
- **Zwei Geräte haben denselben Song geändert:** Beim Songtext fragt die App, welche Fassung gilt, oder behält beide. Einstellungen, Setlists und Tags führt sie selbst zusammen und fragt nur nach, wenn beide dasselbe Feld verschieden gesetzt haben.
