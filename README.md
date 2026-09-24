# chordwright-server

The data server for Chordwright, a songbook app for
ChordPro charts. Point the app at it and your songs stop being rows in a
browser's storage and become `.chordpro` files in a folder — files you can open,
grep, diff, version and sync — shared by every device that connects.

- **Zero dependencies.** `node:http`, `node:fs`, `node:crypto`. Nothing to install.
- **Real files.** One `.chordpro` file per song; everything else pretty-printed JSON.
- **Live.** Edit a song in any editor and an open app updates while you look.
- **Safe for more than one device.** Every record has a revision; a write based
  on an old one is refused with `412` instead of overwriting someone else's edit.
- **Home Assistant add-on** included, with a token and https out of the box.

## Quick start

Pick one.

**Node** (20 or newer):

```sh
node chordwright-data/server/serve.mjs --dir ./data
```

**Docker Compose** — `compose.yaml` in this repository, songs in `./data`:

```sh
docker compose up -d
```

**Home Assistant** — add this repository under *Settings → Add-ons → Add-on
Store → ⋮ → Repositories*, then install **Chordwright Data**. Step by step, in
German: [chordwright-data/DOCS.md](chordwright-data/DOCS.md).

Then in the app: **Einstellungen → Datenquelle**, enter the address (and the
token, if you set one), *Testen*, *Verbinden*. The app reloads and now reads and
writes that folder.

## What the folder looks like

```
data/
  library/
    index.json                    the song entries (title, key, tempo, which file)
    songs/
      lobe-den-herren.chordpro    the file itself — the whole point
      _documents.json             where each file came from
  user/
    songSettings.json             per-song settings: transpose, arrangements, stage cues
    setlists.json
    tags.json
    …                             one file per preference record
```

A key-value dump would have written the ChordPro text JSON-escaped inside a
`.json` file — technically a file, useless to a human. So the key `doc.<id>` is
special-cased into `songs/<id>.chordpro` holding the text and nothing else. The
two fields that are not in the file (`origin`, and the envelope version that
wrote it) live in `_documents.json`, so a round trip through the server changes
nothing. Everything else is stored as pretty-printed JSON, exactly as the app
wrote it.

## What you can do with the folder

- **Edit a song in any editor.** Save it, and an open app updates — file watch →
  server-sent event → the app re-reads that one record.
- **Drop a `.chordpro` file in.** The next time an app starts, it appears as a
  song, with title, key and tempo taken from its own directives.
- **`git init`.** The library is now text under version control, with a
  readable diff per song.
- **rsync, Syncthing, a NAS.** It is a folder.

## Options

| Flag | Default | |
|---|---|---|
| `--dir` | `./data` | the folder to serve |
| `--port` | `4174` | |
| `--host` | `127.0.0.1` | loopback only unless you say otherwise |
| `--token` | none | required as `Authorization: Bearer …` or `?token=` |
| `--insecure` | off | allow a non-loopback host with no token |
| `--cert`, `--key` | none | PEM files; serve https instead of http (both or neither) |

Binding to anything but loopback without a token is **refused**, not warned
about: an open port in a venue's wifi puts every song within reach of anyone on
it, and a warning scrolls past. For a phone on the same network:

```sh
node chordwright-data/server/serve.mjs --dir ./data --host 0.0.0.0 --token $(openssl rand -hex 16)
```

**https:** the app is usually opened over `https://`, and a browser will not let
an https page talk to an `http://` server anywhere but on the same machine
(mixed content). On a network, pass `--cert` and `--key`. The Home Assistant
add-on does this for you — with the certificates in `/ssl`, or a self-signed one.

## API

```
GET    /api/health                → { ok, databases, dir, watching, revisions }   (public)
GET    /api/:db                   → { key: value }
GET    /api/:db?revs=1            → { records: { key: value }, revs: { key: rev } }
GET    /api/:db/record/:key       → { value, rev } | 404 { rev: null }
PUT    /api/:db/record/:key       ← the raw value string            → 204, ETag
DELETE /api/:db/record/:key                                          → 204
GET    /api/events                → SSE: {"db","key","client"} per change
```

`:db` is `library` or `user`. Keys are file names: letters, digits, `.`, `_`,
`-`, starting with a letter or digit. Clients send `X-Chordwright-Client` on
writes and the id comes back on the event, so a client does not react to its own
change. The token goes in the query string for `/api/events`, since
`EventSource` cannot send headers.

### Revisions

Every record has a revision: a hash of its bytes on disk (for a song, of the
ChordPro text alone). Not a timestamp — clocks differ between devices, and a
hash changes by itself when someone edits the file by hand.

`PUT` and `DELETE` can be made conditional:

- `If-Match: "<rev>"` — only if the record is still that revision;
- `If-None-Match: *` — only if it does not exist yet.

If not, nothing is written and the answer is `412` with `{ value, rev }` — what
is there now, so the client can merge or ask. Writing exactly the bytes already
there is never a conflict. Removing what is already gone succeeds. Without
either header a write is unconditional, as it was before revisions existed.

A client should only send these headers once the server has shown that it keeps
revisions — `revisions: true` in `/api/health`, a `rev` in any read, or
`rev: null` on a 404. An older server rejects them in the CORS preflight.

What the Chordwright app does with a `412`: a song text becomes a question to
the user (keep mine, take theirs, keep both); every other record is JSON and is
merged three ways on the revision the write was based on — per key, per `id` in
lists, `tags` as sets — and the user is asked only where both sides changed the
same field differently.

## How it writes and watches

- Every file is written to a temp name and renamed into place, so a reader never
  sees half of one.
- Changes to one record are done one after the other, so a conditional write's
  compare and write cannot interleave with another's.
- `songs/_documents.json` is shared by every song, so changes to it are queued
  too — the first app to connect writes the whole library at once.
- The watcher watches the three directories (`library/`, `library/songs/`,
  `user/`), not the tree recursively: on Linux, Node implements a recursive
  watch per file, and a file replaced by a rename — this server's writes, and
  most editors' saves — silently drops out of it.

## Tests

```sh
npm test
```

`node:test`, no dependencies: the store, conditional writes and their races,
the HTTP API over http and https, live file events, and the add-on's start
script and certificate.

## License

[MIT](LICENSE)
