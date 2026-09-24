/**
 * Einstieg des Home-Assistant-Add-ons: liest die Optionen, die Home Assistant
 * nach /data/options.json schreibt, und startet damit server/serve.mjs.
 *
 * Alles, was man sonst von Hand übergeben müsste, erledigt sich hier selbst:
 * - Ohne Token wird einmal eins erzeugt, in /data/token gemerkt und im Log
 *   gezeigt. Ein Server im Heimnetz ohne Token startet serve.mjs gar nicht erst.
 * - Mit `ssl` und Zertifikaten in /ssl (Let's Encrypt, DuckDNS) spricht er https
 *   mit denen. Fehlen sie, legt er eine eigene kleine Zertifizierungsstelle an
 *   und stellt sich damit ein Zertifikat für seine eigenen Adressen aus. Die CA
 *   installiert man einmal pro Gerät (sie liegt unter /ca.crt und in share) —
 *   danach vertraut jeder Browser dem Server, auch eine App vom Home-Bildschirm.
 *
 * Die Pfade sind über Umgebungsvariablen umbiegbar — nur für die Tests.
 */

import { spawn } from 'node:child_process';
import { randomBytes, X509Certificate } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createCA, issueServerCert } from './certs.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const DATA = process.env.ADDON_DATA_DIR ?? '/data';
const SHARE = process.env.ADDON_SHARE_DIR ?? '/share';
const SSL = process.env.ADDON_SSL_DIR ?? '/ssl';
const PORT = process.env.ADDON_PORT ?? '4174';
const SUPERVISOR = process.env.ADDON_SUPERVISOR_URL ?? 'http://supervisor';

/** Namen, die jedes Zertifikat trägt — die üblichen Adressen eines Home Assistant. */
const DEFAULT_NAMES = ['homeassistant.local', 'homeassistant', 'localhost'];
const DAY = 24 * 60 * 60 * 1000;

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

/**
 * Die IP-Adressen des Home-Assistant-Rechners. Das Add-on selbst läuft in
 * einem Container und sieht sie nicht; der Supervisor kennt sie. Scheitert die
 * Frage, geht es ohne weiter — dann tragen nur die Namen.
 */
async function hostAddresses() {
  const token = process.env.SUPERVISOR_TOKEN;
  if (!token && !process.env.ADDON_SUPERVISOR_URL) return [];
  try {
    const res = await fetch(`${SUPERVISOR}/network/info`, {
      headers: token ? { authorization: `Bearer ${token}` } : {},
      signal: AbortSignal.timeout(3000),
    });
    const body = await res.json();
    const ips = [];
    for (const iface of body?.data?.interfaces ?? []) {
      for (const family of ['ipv4', 'ipv6']) {
        for (const address of iface?.[family]?.address ?? []) {
          const ip = String(address).split('/')[0];
          // Link-local IPv6 taugt nicht als Adresse, die jemand eintippt.
          if (ip && !ip.toLowerCase().startsWith('fe80')) ips.push(ip);
        }
      }
    }
    return ips;
  } catch {
    return [];
  }
}

const isIp = (s) => /^\d{1,3}(\.\d{1,3}){3}$/.test(s) || s.includes(':');

function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, 'utf8'));
  } catch {
    return null;
  }
}

function validFor(certPath, days) {
  try {
    return Date.parse(new X509Certificate(readFileSync(certPath)).validTo) - Date.now() > days * DAY;
  } catch {
    return false;
  }
}

async function resolveTls(options) {
  if (options.ssl === false) return null;
  const cert = join(SSL, options.certfile ?? 'fullchain.pem');
  const key = join(SSL, options.keyfile ?? 'privkey.pem');
  if (existsSync(cert) && existsSync(key)) return { cert, key, own: false };

  mkdirSync(DATA, { recursive: true });

  // Die CA: einmal angelegt, dann für immer dieselbe — sie ist es, die auf den
  // Geräten installiert ist. Nur eine, die bald abläuft, wird ersetzt.
  const ca = { cert: join(DATA, 'ca-cert.pem'), key: join(DATA, 'ca-key.pem') };
  let caRenewed = false;
  if (!existsSync(ca.key) || !validFor(ca.cert, 60)) {
    const fresh = createCA();
    writeFileSync(ca.key, fresh.key, { mode: 0o600 });
    writeFileSync(ca.cert, fresh.cert);
    caRenewed = true;
  }

  // Das Server-Zertifikat: für jede Adresse, unter der man ihn erreicht. Ändert
  // sich eine (neue IP im Router), wird es neu ausgestellt — die CA bleibt.
  const configured = (options.hostnames ?? []).map((h) => String(h).trim()).filter(Boolean);
  const all = [...new Set([...DEFAULT_NAMES, '127.0.0.1', ...(await hostAddresses()), ...configured])];
  const names = all.filter((h) => !isIp(h));
  const ips = all.filter(isIp);
  const wanted = JSON.stringify({ names, ips });

  const own = { cert: join(DATA, 'server-cert.pem'), key: join(DATA, 'server-key.pem'), meta: join(DATA, 'server-cert.json') };
  const current = readJson(own.meta);
  if (caRenewed || !existsSync(own.key) || !validFor(own.cert, 30) || JSON.stringify(current) !== wanted) {
    const fresh = issueServerCert(
      { cert: readFileSync(ca.cert, 'utf8'), key: readFileSync(ca.key, 'utf8') },
      { dnsNames: names, ips },
    );
    writeFileSync(own.key, fresh.key, { mode: 0o600 });
    writeFileSync(own.cert, fresh.cert);
    writeFileSync(own.meta, wanted + '\n');
  }
  return { cert: own.cert, key: own.key, ca: ca.cert, own: true, names, ips };
}

const options = readOptions();
const folder = options.folder || 'chordwright';
const dir = join(SHARE, folder);
const { token, generated } = resolveToken(options.token);
const tls = await resolveTls(options);
const scheme = tls ? 'https' : 'http';

// Die CA auch als Datei in share: am Mac per Samba in den Schlüsselbund ziehen.
if (tls?.own) {
  try {
    mkdirSync(dir, { recursive: true });
    copyFileSync(tls.ca, join(dir, 'chordwright-ca.crt'));
  } catch {
    /* ohne Datei geht es auch — /ca.crt gibt es trotzdem */
  }
}

const addresses = tls?.own ? [...tls.ips.filter((ip) => ip !== '127.0.0.1'), 'homeassistant.local'] : [];
const rule = '-'.repeat(60);
console.log(rule);
console.log('Chordwright Data');
console.log(`  Ordner   ${dir}  (per Samba: share/${folder})`);
if (addresses.length > 0) {
  for (const a of addresses) console.log(`  Adresse  ${scheme}://${a.includes(':') ? `[${a}]` : a}:${PORT}`);
} else {
  console.log(`  Adresse  ${scheme}://<deine-Home-Assistant-Adresse>:${PORT}`);
}
console.log(`  Token    ${token}${generated ? '   (automatisch erzeugt)' : ''}`);
if (tls?.own) {
  console.log('  Zertifikat: von der eigenen Chordwright-CA — einmal pro Gerät installieren:');
  console.log(`    ${scheme}://${addresses[0] ?? '<Adresse>'}:${PORT}/ca.crt   (oder per Samba: share/${folder}/chordwright-ca.crt)`);
  console.log(`  Gilt für: ${[...tls.names, ...tls.ips].join(', ')}`);
}
if (!tls) console.log('  Ohne ssl erreicht die App auf GitHub Pages (https) den Server nicht.');
console.log('In der App: Einstellungen → Datenquelle → Adresse (ohne /api) und Token eintragen.');
console.log(rule);

const serverArgs = [
  join(here, 'server', 'serve.mjs'),
  '--dir', dir,
  '--port', PORT,
  '--host', '0.0.0.0',
  '--token', token,
];
if (tls) serverArgs.push('--cert', tls.cert, '--key', tls.key);
if (tls?.own) serverArgs.push('--ca-file', tls.ca);

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
