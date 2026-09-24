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

In Chordwright: **Einstellungen → Datenquelle**

- **Adresse:** `https://homeassistant.local:4174`. Nimm dieselbe Adresse, unter der du Home Assistant erreichst, aber mit Port `4174` und `https`.
- **Token:** den aus dem Protokoll
- **Testen**, dann **Verbinden**. Die App lädt neu und liest und schreibt ab jetzt den Ordner auf dem Home-Assistant-Rechner.

Verbindet sich die erste App mit dem leeren Ordner, schreibt sie ihre Songs hinein. Mach das also von dem Gerät aus, auf dem deine Bibliothek gerade liegt. Jedes weitere Gerät verbindest du genauso.

Chrome fragt beim ersten Mal, ob die Seite auf Geräte im lokalen Netzwerk zugreifen darf. Das mit **Zulassen** bestätigen.

## Das Zertifikat

Die App läuft auf GitHub Pages, also über `https`. Browser lassen eine https-Seite nicht mit einem `http`-Server reden. Darum spricht das Add-on selbst https. Dabei gibt es zwei Fälle:

**Du hast schon Zertifikate in Home Assistant** (Add-on „Let's Encrypt" oder „Duck DNS"): Die liegen in `/ssl` als `fullchain.pem` und `privkey.pem`, und das Add-on nimmt sie automatisch. Als Adresse trägst du dann den Namen ein, für den das Zertifikat gilt, z. B. `https://deinname.duckdns.org:4174`. Heißen die Dateien anders, trag die Namen unter **Konfiguration** ein.

**Du hast keine** (der Normalfall, Home Assistant unter `http://homeassistant.local:8123`): Dann erzeugt das Add-on ein eigenes, selbstsigniertes Zertifikat. Das musst du **auf jedem Gerät einmal bestätigen**:

1. Im Browser `https://homeassistant.local:4174/api/health` öffnen.
2. Die Warnung bestätigen: Chrome *Erweitert → Weiter zu …*, Safari *Details einblenden → Diese Website besuchen*.
3. Es erscheint eine kurze Zeile mit `"ok":true`. Danach kann die App verbinden.

Auf dem iPhone merkt sich Safari diese Bestätigung nicht immer dauerhaft. Wenn die App später „offline" zeigt, Schritt 1 und 2 wiederholen. Wer das Add-on regelmäßig auf dem iPhone nutzt, fährt mit einem echten Zertifikat (Duck DNS) besser.

## Konfiguration

| Option | Standard | |
|---|---|---|
| `token` | leer | Leer lassen: Dann wird einmal einer erzeugt und gemerkt. Eigenen setzen: Dann gilt der. |
| `ssl` | an | Aus nur, wenn die App selbst über `http` läuft (z. B. `npm run dev` im LAN). |
| `certfile` / `keyfile` | `fullchain.pem` / `privkey.pem` | Dateinamen in `/ssl` |
| `folder` | `chordwright` | Unterordner in `share`, in dem die Bibliothek liegt |

Den Port änderst du unter **Netzwerk**. In der App trägst du dann diesen Port ein.

## Die Songs als Dateien

Per Samba unter **share → chordwright**:

```
library/songs/*.chordpro   die Songs, in jedem Editor bearbeitbar
library/index.json         Titel, Tonart, Tempo
user/                      Capo, Abläufe, Setlists, Tags
```

Speicherst du einen Song, sieht eine offene App die Änderung sofort. Legst du eine neue `.chordpro`-Datei ab, erscheint sie beim nächsten Start der App als Song.

**Backup:** Die Songs landen in Home-Assistant-Sicherungen, wenn darin der Ordner „Share" mitgesichert wird.

## Aktualisieren

Neue Versionen erscheinen im Add-on-Store wie bei jedem anderen Add-on; **Aktualisieren** klicken. Die Songs, der Token und das Zertifikat bleiben erhalten.

## Ohne Zugriff auf GitHub: als lokales Add-on

Falls Home Assistant das Repository nicht erreicht (etwa weil es privat ist): per Samba die Freigabe **addons** öffnen, den Ordner `chordwright-data` aus diesem Repository hineinziehen, sodass dort `addons/chordwright-data/config.yaml` liegt. Dann im Add-on-Store **⋮ → Nach Updates suchen**; das Add-on erscheint unter **Lokale Add-ons**. Für ein Update den Ordner erneut kopieren.

## Wenn etwas nicht geht

- **„Testen" schlägt fehl:** Läuft das Add-on (Protokoll)? Hast du das Zertifikat auf diesem Gerät bestätigt? Steht in der Adresse `https`?
- **Der Token wird abgelehnt:** Den Token noch einmal aus dem Protokoll kopieren, ohne Leerzeichen.
- **Zwei Geräte haben denselben Song geändert:** Beim Songtext fragt die App, welche Fassung gilt, oder behält beide. Einstellungen, Setlists und Tags führt sie selbst zusammen und fragt nur nach, wenn beide dasselbe Feld verschieden gesetzt haben.
