/**
 * Einstieg des Home-Assistant-Add-ons: liest die Optionen, die Home Assistant
 * nach /data/options.json schreibt, und startet damit server/serve.mjs.
 *
 * Alles, was man sonst von Hand übergeben müsste, erledigt sich hier selbst:
 * - Ohne Token wird einmal eins erzeugt, in /data/token gemerkt und im Log
 *   gezeigt. Ein Server im Heimnetz ohne Token startet serve.mjs gar nicht erst.
 * - Mit `ssl` und Zertifikaten in /ssl (Let's Encrypt, DuckDNS) spricht er https
 *   mit denen. Fehlen sie, erzeugt er ein selbstsigniertes, damit die App von
 *   GitHub Pages (https) ihn trotzdem erreichen kann.
 *
 * Die Pfade sind über Umgebungsvariablen umbiegbar — nur für die Tests.
 */

import { spawn } from 'node:child_process';
import { randomBytes, X509Certificate } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createSelfSigned } from './selfsigned.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const DATA = process.env.ADDON_DATA_DIR ?? '/data';
const SHARE = process.env.ADDON_SHARE_DIR ?? '/share';
const SSL = process.env.ADDON_SSL_DIR ?? '/ssl';
const PORT = process.env.ADDON_PORT ?? '4174';

function readOptions() {
  try {
    return JSON.parse(readFileSync(join(DATA, 'options.json'), 'utf8'));
  } catch {
    return {};
  }
}

function resolveToken(configured) {
  if (configured) return { token: configured, generated: false };
  const file = join(DATA, 'token');
  if (existsSync(file)) {
    const saved = readFileSync(file, 'utf8').trim();
    if (saved) return { token: saved, generated: true };
  }
  const token = randomBytes(16).toString('hex');
  mkdirSync(DATA, { recursive: true });
  writeFileSync(file, token + '\n', { mode: 0o600 });
  return { token, generated: true };
}

function resolveTls(options) {
  if (options.ssl === false) return null;
  const cert = join(SSL, options.certfile ?? 'fullchain.pem');
  const key = join(SSL, options.keyfile ?? 'privkey.pem');
  if (existsSync(cert) && existsSync(key)) return { cert, key, selfSigned: false };

  const own = { cert: join(DATA, 'selfsigned-cert.pem'), key: join(DATA, 'selfsigned-key.pem'), selfSigned: true };
  if (!usable(own)) {
    const fresh = createSelfSigned();
    mkdirSync(DATA, { recursive: true });
    writeFileSync(own.key, fresh.key, { mode: 0o600 });
    writeFileSync(own.cert, fresh.cert);
  }
  return own;
}

/** Da, lesbar, und noch mindestens 30 Tage gültig — sonst ein neues. */
function usable({ cert, key }) {
  if (!existsSync(cert) || !existsSync(key)) return false;
  try {
    const validTo = Date.parse(new X509Certificate(readFileSync(cert)).validTo);
    return validTo - Date.now() > 30 * 24 * 60 * 60 * 1000;
  } catch {
    return false;
  }
}

const options = readOptions();
const folder = options.folder || 'chordwright';
const dir = join(SHARE, folder);
const { token, generated } = resolveToken(options.token);
const tls = resolveTls(options);
const scheme = tls ? 'https' : 'http';

const rule = '-'.repeat(60);
console.log(rule);
console.log('Chordwright Data');
console.log(`  Ordner   ${dir}  (per Samba: share/${folder})`);
console.log(`  Adresse  ${scheme}://<deine-Home-Assistant-Adresse>:<Port>   (Standard-Port ${PORT})`);
console.log(`  Token    ${token}${generated ? '   (automatisch erzeugt)' : ''}`);
if (tls?.selfSigned) {
  console.log('  Zertifikat: selbstsigniert — keins in /ssl gefunden.');
  console.log(`  Einmal ${scheme}://<Adresse>:<Port>/api/health im Browser öffnen und bestätigen.`);
  console.log(`  Neu erzeugt wird es nur, wenn es in unter 30 Tagen abläuft.`);
}
if (!tls) console.log('  Ohne ssl erreicht die App auf GitHub Pages (https) den Server nicht.');
console.log('In der App: Einstellungen → Datenquelle → Adresse und Token eintragen.');
console.log(rule);

const serverArgs = [
  join(here, 'server', 'serve.mjs'),
  '--dir', dir,
  '--port', PORT,
  '--host', '0.0.0.0',
  '--token', token,
];
if (tls) serverArgs.push('--cert', tls.cert, '--key', tls.key);

const child = spawn(process.execPath, serverArgs, { stdio: 'inherit' });
// Home Assistant stoppt mit SIGTERM; das ist ein gewolltes Ende, kein Absturz.
let stopping = false;
for (const signal of ['SIGTERM', 'SIGINT']) {
  process.on(signal, () => {
    stopping = true;
    child.kill(signal);
  });
}
child.on('exit', (code) => process.exit(stopping ? 0 : (code ?? 1)));
