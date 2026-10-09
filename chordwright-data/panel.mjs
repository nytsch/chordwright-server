/**
 * Die Web-UI des Add-ons (Home Assistant: „Web-UI öffnen", per Ingress): ein
 * QR-Code, mit dem sich ein Gerät verbindet, und bei eigener CA einer fürs
 * Zertifikat davor. Mehr nicht — eingestellt wird alles unter „Konfiguration".
 *
 * Die Seite zeigt den Token. Das darf sie, weil Home Assistant sie nur
 * angemeldeten Nutzern ausliefert (wie das Protokoll, in dem er auch steht);
 * deshalb nimmt der Server nur Anfragen von Home Assistants Ingress-Proxy an.
 */

import { createServer } from 'node:http';
import { encode } from './qr.mjs';

/**
 * Wohin der QR-Code führt: die Web-App, gleich auf der Datenquellen-Seite, mit
 * Adresse und Token schon eingetragen. Dasselbe wie `connectLink` in der App
 * (app/src/app/localLibrary.ts); die App liest beides auch aus ihrem eigenen
 * Scanner. Bis es eine stabile Fassung gibt, ist das die Beta.
 */
export const APP_URL = 'https://beta.chordwright.app/';

export function connectLink(server, token) {
  const query = new URLSearchParams({ server, token });
  return `${APP_URL}#/settings/source?${query}`;
}

/** Eine Adresse als URL — IPv6 in eckigen Klammern. */
export function addressUrl(scheme, address, port) {
  return `${scheme}://${address.includes(':') ? `[${address}]` : address}:${port}`;
}

/**
 * Welche Adresse zuerst kommt: IPv4 (geht überall), dann Namen (`.local` löst
 * nicht jedes Android auf), IPv6 zuletzt.
 */
export function orderAddresses(addresses) {
  const rank = (a) => (/^\d+(\.\d+){3}$/.test(a) ? 0 : a.includes(':') ? 2 : 1);
  return [...new Set(addresses)].sort((a, b) => rank(a) - rank(b));
}

const esc = (s) =>
  String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);

/** Schwarz auf weiß in jedem Theme: manche Kameras lesen einen hellen Code auf dunkel nicht. */
function qrSvg(text, label) {
  const qr = encode(text, { ecc: 'M', border: 2 });
  const cells = [];
  qr.data.forEach((row, y) => row.forEach((dark, x) => dark && cells.push(`M${x} ${y}h1v1h-1z`)));
  return (
    `<svg class="qr" role="img" aria-label="${esc(label)}" viewBox="0 0 ${qr.size} ${qr.size}" shape-rendering="crispEdges">` +
    `<rect width="${qr.size}" height="${qr.size}" fill="#fff"/><path d="${cells.join('')}" fill="#000"/></svg>`
  );
}

/**
 * Die Seite. `pick` ist die Nummer der gewählten Adresse (`?a=`), falls es
 * mehrere gibt — der Code gilt immer für genau eine.
 */
export function panelPage({ addresses, scheme, port, token, ownCa, pick = 0 }) {
  const list = orderAddresses(addresses);
  const chosen = list[pick] ?? list[0];
  const server = chosen ? addressUrl(scheme, chosen, port) : null;

  const chips =
    list.length > 1
      ? `<nav class="chips" aria-label="Adresse">${list
          .map((a, i) => `<a href="?a=${i}"${a === chosen ? ' aria-current="true"' : ''}>${esc(a)}</a>`)
          .join('')}</nav>`
      : '';

  let body;
  if (!server) {
    body =
      '<p class="note">Die Adresse dieses Rechners ist unbekannt. Trag unter <b>Konfiguration → hostnames</b> ' +
      'den Namen ein, unter dem du Home Assistant erreichst, und starte das Add-on neu.</p>';
  } else {
    const ca = `${server}/ca.crt`;
    const link = connectLink(server, token);
    const caStep = ownCa
      ? `<section>
  <h2><span class="n">1</span>Zertifikat <small>einmal pro Gerät</small></h2>
  ${qrSvg(ca, 'QR-Code: Zertifikat laden')}
  <p>Mit der Kamera scannen und öffnen. Am iPhone danach unter <b>Einstellungen → Allgemein → Info →
  Zertifikatsvertrauenseinstellungen</b> „Chordwright lokale CA“ einschalten.</p>
  <p class="value"><a href="${esc(ca)}" target="_blank" rel="noreferrer">${esc(ca)}</a></p>
</section>`
      : '';
    body = `${caStep}
<section>
  <h2>${ownCa ? '<span class="n">2</span>' : ''}Verbinden</h2>
  ${qrSvg(link, 'QR-Code: Chordwright mit diesem Server verbinden')}
  <p>Mit der Kamera scannen, oder in Chordwright unter <b>Einstellungen → Datenquelle → Server hinzufügen</b>
  „QR-Code scannen“. Auf diesem Gerät: <a class="connect" href="${esc(link)}" target="_blank" rel="noreferrer">hier öffnen</a>.</p>
  <dl>
    <dt>Adresse</dt><dd class="value">${esc(server)}</dd>
    <dt>Token</dt><dd class="value">${esc(token)}</dd>
  </dl>
</section>`;
  }

  return `<!doctype html>
<html lang="de">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Chordwright Data</title>
<style>
  :root { color-scheme: light dark; --fg: #1c1c1e; --muted: #6b6b70; --line: #d9d9de; --accent: #0a7c6b; --bg: #fafafa; }
  @media (prefers-color-scheme: dark) { :root { --fg: #ececf0; --muted: #a0a0a8; --line: #3a3a40; --accent: #4cc7b2; --bg: #111214; } }
  body { margin: 0; padding: 24px 16px; background: var(--bg); color: var(--fg); font: 15px/1.45 system-ui, sans-serif; }
  main { max-width: 760px; margin: 0 auto; }
  h1 { font-size: 20px; margin: 0 0 16px; }
  .steps { display: flex; flex-wrap: wrap; gap: 24px; }
  section { flex: 1 1 280px; min-width: 0; }
  h2 { font-size: 16px; margin: 0 0 12px; display: flex; align-items: center; gap: 8px; }
  h2 small { color: var(--muted); font-weight: normal; }
  .n { display: inline-grid; place-items: center; width: 22px; height: 22px; border-radius: 50%; background: var(--accent); color: var(--bg); font-size: 13px; }
  .qr { display: block; width: 100%; max-width: 260px; height: auto; border-radius: 8px; }
  p { margin: 12px 0; }
  dl { display: grid; grid-template-columns: auto 1fr; gap: 4px 12px; margin: 12px 0; }
  dt { color: var(--muted); }
  dd { margin: 0; }
  .value { font-family: ui-monospace, monospace; font-size: 13px; overflow-wrap: anywhere; }
  a { color: var(--accent); }
  .chips { display: flex; flex-wrap: wrap; gap: 8px; margin: 0 0 20px; }
  .chips a { padding: 6px 12px; border: 1px solid var(--line); border-radius: 999px; text-decoration: none; color: var(--fg); font-size: 13px; }
  .chips a[aria-current] { border-color: var(--accent); color: var(--accent); }
  .note { color: var(--muted); }
</style>
</head>
<body>
<main>
<h1>Chordwright Data</h1>
${chips}
<div class="steps">
${body}
</div>
</main>
</body>
</html>
`;
}

/** Nur Home Assistants Ingress-Proxy (und der eigene Rechner, für die Tests). */
const ALLOWED = new Set(['172.30.32.2', '::ffff:172.30.32.2', '127.0.0.1', '::ffff:127.0.0.1', '::1']);

/** Startet die Web-UI. `page(pick)` liefert das HTML. */
export function startPanel({ port, page }) {
  const server = createServer((req, res) => {
    if (!ALLOWED.has(req.socket.remoteAddress)) {
      res.writeHead(403).end();
      return;
    }
    const url = new URL(req.url ?? '/', 'http://panel');
    if (req.method !== 'GET' || url.pathname !== '/') {
      res.writeHead(404).end();
      return;
    }
    const pick = Number.parseInt(url.searchParams.get('a') ?? '0', 10) || 0;
    res.writeHead(200, {
      'content-type': 'text/html; charset=utf-8',
      'cache-control': 'no-store',
      'referrer-policy': 'no-referrer',
    });
    res.end(page(pick));
  });
  server.listen(Number(port), '0.0.0.0');
  return server;
}
