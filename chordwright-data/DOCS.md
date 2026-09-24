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
| `hostnames` | leer | Weitere Namen oder Adressen fürs Zertifikat, z. B. `ha.fritz.box` |

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

- **„Testen" schlägt fehl:** Läuft das Add-on (Protokoll)? Ist die Zertifizierungsstelle auf diesem Gerät installiert und (iPhone) in den Zertifikatsvertrauenseinstellungen eingeschaltet? Steht die Adresse im Protokoll unter „Gilt für"? Steht in der Adresse `https`?
- **Der Token wird abgelehnt:** Den Token noch einmal aus dem Protokoll kopieren, ohne Leerzeichen.
- **Zwei Geräte haben denselben Song geändert:** Beim Songtext fragt die App, welche Fassung gilt, oder behält beide. Einstellungen, Setlists und Tags führt sie selbst zusammen und fragt nur nach, wenn beide dasselbe Feld verschieden gesetzt haben.
