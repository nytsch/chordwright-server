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
import { randomBytes } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { X509Certificate } from 'node:crypto';
import { ensureOwnCertificate } from './certs.mjs';
import { addressUrl, orderAddresses, panelPage, startPanel } from './panel.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const DATA = process.env.ADDON_DATA_DIR ?? '/data';
const SHARE = process.env.ADDON_SHARE_DIR ?? '/share';
const SSL = process.env.ADDON_SSL_DIR ?? '/ssl';
const PORT = process.env.ADDON_PORT ?? '4174';
const SUPERVISOR = process.env.ADDON_SUPERVISOR_URL ?? 'http://supervisor';
// Die Web-UI (Ingress), wie in config.yaml unter ingress_port.
const INGRESS_PORT = process.env.ADDON_INGRESS_PORT ?? '8099';

/** Namen, die jedes Zertifikat trägt — die üblichen Adressen eines Home Assistant. */
const DEFAULT_NAMES = ['homeassistant.local', 'homeassistant', 'localhost'];

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

/** Eine Frage an den Supervisor; scheitert sie, `null` — dann geht es ohne. */
async function supervisor(path) {
  const token = process.env.SUPERVISOR_TOKEN;
  if (!token && !process.env.ADDON_SUPERVISOR_URL) return null;
  try {
    const res = await fetch(`${SUPERVISOR}${path}`, {
      headers: token ? { authorization: `Bearer ${token}` } : {},
      signal: AbortSignal.timeout(3000),
    });
    return (await res.json())?.data ?? null;
  } catch {
    return null;
  }
}

/**
 * Die IP-Adressen des Home-Assistant-Rechners. Das Add-on selbst läuft in
 * einem Container und sieht sie nicht; der Supervisor kennt sie. Scheitert die
 * Frage, geht es ohne weiter — dann tragen nur die Namen.
 */
async function hostAddresses() {
  const ips = [];
  for (const iface of (await supervisor('/network/info'))?.interfaces ?? []) {
    for (const family of ['ipv4', 'ipv6']) {
      for (const address of iface?.[family]?.address ?? []) {
        const ip = String(address).split('/')[0];
        // Link-local IPv6 taugt nicht als Adresse, die jemand eintippt.
        if (ip && !ip.toLowerCase().startsWith('fe80')) ips.push(ip);
      }
    }
  }
  return ips;
}

/**
 * Der Port, unter dem man das Add-on von außen erreicht: unter „Netzwerk“
 * umstellbar, der Container hört trotzdem auf PORT.
 */
async function hostPort() {
  const mapped = (await supervisor('/addons/self/info'))?.network?.[`${PORT}/tcp`];
  return mapped ? String(mapped) : PORT;
}

/** Die Namen in einem Zertifikat (Let's Encrypt, DuckDNS) — ohne Platzhalter wie *.example.org. */
function certificateNames(file) {
  try {
    const san = new X509Certificate(readFileSync(file)).subjectAltName ?? '';
    return san
      .split(',')
      .map((part) => part.trim())
      .filter((part) => part.startsWith('DNS:') && !part.includes('*'))
      .map((part) => part.slice(4));
  } catch {
    return [];
  }
}

async function resolveTls(options, ips) {
  if (options.ssl === false) return null;
  const cert = join(SSL, options.certfile ?? 'fullchain.pem');
  const key = join(SSL, options.keyfile ?? 'privkey.pem');
  if (existsSync(cert) && existsSync(key)) return { cert, key, own: false, names: certificateNames(cert) };

  // Das Zertifikat: für jede Adresse, unter der man ihn erreicht. Aus den
  // Optionen (Home Assistant) oder CHORDWRIGHT_HOSTNAMES (docker compose).
  // CA und Erneuerung: ensureOwnCertificate in certs.mjs.
  const fromEnv = (process.env.CHORDWRIGHT_HOSTNAMES ?? '').split(',');
  const configured = [...(options.hostnames ?? []), ...fromEnv];
  const own = ensureOwnCertificate(DATA, [...DEFAULT_NAMES, '127.0.0.1', ...ips, ...configured]);
  return { ...own, own: true };
}

const options = readOptions();
const folder = options.folder || 'chordwright';
const dir = join(SHARE, folder);
const { token, generated } = resolveToken(options.token);
const ips = await hostAddresses();
const tls = await resolveTls(options, ips);
const scheme = tls ? 'https' : 'http';
const port = await hostPort();

// Die CA auch als Datei in share: am Mac per Samba in den Schlüsselbund ziehen.
if (tls?.own) {
  try {
    mkdirSync(dir, { recursive: true });
    copyFileSync(tls.ca, join(dir, 'chordwright-ca.crt'));
  } catch {
    /* ohne Datei geht es auch — /ca.crt gibt es trotzdem */
  }
}

// Unter Home Assistant gibt es den Supervisor-Token; sonst (docker compose,
// von Hand) ist es der eigene Rechner, und Samba gibt es nicht.
const inHomeAssistant = Boolean(process.env.SUPERVISOR_TOKEN);
const hostnames = (options.hostnames ?? []).map((h) => String(h).trim()).filter(Boolean);
const local = inHomeAssistant ? 'homeassistant.local' : 'localhost';
// Wo man ihn erreicht: mit eigener CA alles, was im Zertifikat steht; mit einem
// fremden die Namen darin; ohne https die Adressen des Rechners.
const addresses = orderAddresses(
  tls?.own
    ? [...tls.ips.filter((ip) => ip !== '127.0.0.1'), local, ...hostnames]
    : tls
      ? [...tls.names, ...hostnames]
      : [...ips, local, ...hostnames],
);
const rule = '-'.repeat(60);
console.log(rule);
console.log('Chordwright Data');
console.log(`  Ordner   ${dir}${inHomeAssistant ? `  (per Samba: share/${folder})` : ''}`);
if (addresses.length > 0) {
  for (const a of addresses) console.log(`  Adresse  ${addressUrl(scheme, a, port)}`);
} else {
  console.log(`  Adresse  ${scheme}://<deine-Home-Assistant-Adresse>:${port}`);
}
console.log(`  Token    ${token}${generated ? '   (automatisch erzeugt)' : ''}`);
if (tls?.own) {
  console.log('  Zertifikat: von der eigenen Chordwright-CA — einmal pro Gerät installieren:');
  const caFile = inHomeAssistant ? `per Samba: share/${folder}/chordwright-ca.crt` : 'oder die Datei chordwright-ca.crt im Datenordner';
  console.log(`    ${addresses[0] ? addressUrl(scheme, addresses[0], port) : `${scheme}://<Adresse>:${port}`}/ca.crt   (${caFile})`);
  console.log(`  Gilt für: ${[...tls.names, ...tls.ips].join(', ')}`);
}
if (!tls) console.log('  Ohne ssl erreicht die App auf GitHub Pages (https) den Server nicht.');
console.log('In der App: Einstellungen → Datenquelle → Adresse (ohne /api) und Token eintragen.');
if (inHomeAssistant) console.log('Oder per QR-Code: oben beim Add-on „Web-UI öffnen“.');
console.log(rule);

// Die Web-UI mit dem QR-Code. Unter Home Assistant immer; sonst (Tests) nur,
// wenn jemand einen Port dafür nennt.
if (inHomeAssistant || process.env.ADDON_INGRESS_PORT) {
  startPanel({
    port: INGRESS_PORT,
    page: (pick) => panelPage({ addresses, scheme, port, token, ownCa: Boolean(tls?.own), pick }),
  }).on('error', (err) => console.error(`Web-UI startet nicht: ${err.message}`));
}

const serverArgs = [
  join(here, 'server', 'serve.mjs'),
  '--dir', dir,
  '--port', PORT,
  '--host', '0.0.0.0',
  '--token', token,
];
// Ältere Optionen kennen die beiden nicht; dann gilt der Standard des Servers.
if (options.backup_every_hours !== undefined) serverArgs.push('--backup-every', String(options.backup_every_hours));
if (options.backup_keep !== undefined) serverArgs.push('--backup-keep', String(options.backup_keep));
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
