#!/usr/bin/env node
/**
 * Der Server als eine einzige ausführbare Datei, ohne installiertes Node: das,
 * was die Chordwright-Desktop-App mitliefert und startet, wenn die Bibliothek
 * ein Ordner auf demselben Rechner sein soll.
 *
 *   node scripts/build-binary.mjs                          für diesen Rechner
 *   node scripts/build-binary.mjs --target x86_64-apple-darwin
 *
 * Ergebnis: dist/chordwright-server-<target>[.exe] — benannt nach dem
 * Rust-Target, weil Tauri eine mitgelieferte Programmdatei genau unter diesem
 * Namen sucht (`bundle.externalBin`).
 *
 * Gebaut wird eine Node Single Executable Application: das Node, das ohnehin
 * läuft, mit serve.mjs darin. Nicht Bun, obwohl dessen Dateien kleiner wären:
 * Buns fs.watch meldet ein Speichern per Umbenennen nur unter dem alten Namen,
 * und genau so speichern dieser Server und die meisten Editoren — die App
 * bekäme von einer Änderung am Lied nichts mit. Mit Node läuft im Paket
 * dieselbe Laufzeit wie unter `npm test`.
 *
 * Für ein anderes Ziel als diesen Rechner lädt das Skript das Node derselben
 * Version für dieses Ziel von nodejs.org. macOS-Ziele nur auf einem Mac: das
 * Ergebnis muss neu signiert werden, sonst startet es auf Apple Silicon nicht.
 *
 * Zum Bauen per npx: esbuild (serve.mjs und seine Module zu einer CommonJS-
 * Datei, denn eine SEA nimmt nur eine) und postject (setzt sie ins Node ein).
 * Der Server selbst bleibt ohne Abhängigkeiten.
 */

import { spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ESBUILD = 'esbuild@0.25.10';
const POSTJECT = 'postject@1.0.0-alpha.6';
// Fest in jedem Node, das SEAs kann; postject sucht danach.
const FUSE = 'NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2';

/** Rust-Target → das Node-Paket auf nodejs.org und wo darin das Programm liegt. */
const TARGETS = {
  'aarch64-apple-darwin': { os: 'darwin', node: 'darwin-arm64' },
  'x86_64-apple-darwin': { os: 'darwin', node: 'darwin-x64' },
  'x86_64-pc-windows-msvc': { os: 'win32', node: 'win-x64' },
  'aarch64-pc-windows-msvc': { os: 'win32', node: 'win-arm64' },
  'x86_64-unknown-linux-gnu': { os: 'linux', node: 'linux-x64' },
  'aarch64-unknown-linux-gnu': { os: 'linux', node: 'linux-arm64' },
};

const HOST = {
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'win32-x64': 'x86_64-pc-windows-msvc',
  'win32-arm64': 'aarch64-pc-windows-msvc',
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'linux-arm64': 'aarch64-unknown-linux-gnu',
}[`${process.platform}-${process.arch}`];

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const dist = join(root, 'dist');

function fail(message) {
  console.error(message);
  process.exit(1);
}

function run(cmd, args, opts = {}) {
  console.log(`$ ${cmd} ${args.join(' ')}`);
  // npx ist unter Windows npx.cmd; das findet nur eine Shell.
  const result = spawnSync(cmd, args, { stdio: 'inherit', shell: process.platform === 'win32', ...opts });
  if (result.status !== 0) fail(`${cmd} ist gescheitert (${result.status ?? result.signal}).`);
}

const flag = process.argv.indexOf('--target');
const target = flag >= 0 ? process.argv[flag + 1] : HOST;
const spec = TARGETS[target];
if (!spec) fail(`Unbekanntes Ziel ${target}. Bekannt: ${Object.keys(TARGETS).join(', ')}`);
if (spec.os === 'darwin' && process.platform !== 'darwin') {
  fail('macOS-Ziele nur auf einem Mac bauen: das Ergebnis muss mit codesign neu signiert werden.');
}

mkdirSync(dist, { recursive: true });
const exe = spec.os === 'win32' ? '.exe' : '';
const out = join(dist, `chordwright-server-${target}${exe}`);

// 1. Ein Skript. Ohne Code-Cache: der wäre an dieses Node gebunden, das Paket
//    soll aber auch auf einem anderen Ziel als diesem Rechner laufen.
const bundle = join(dist, 'serve.cjs');
run('npx', [
  '--yes', ESBUILD, join(root, 'chordwright-data', 'server', 'serve.mjs'),
  '--bundle', '--platform=node', '--format=cjs', '--target=node22', `--outfile=${bundle}`, '--log-level=warning',
]);
const blob = join(dist, 'sea.blob');
const config = join(dist, 'sea-config.json');
writeFileSync(
  config,
  JSON.stringify({ main: bundle, output: blob, disableExperimentalSEAWarning: true, useCodeCache: false, useSnapshot: false }),
);
run(process.execPath, ['--experimental-sea-config', config]);

// 2. Das Node, in das es kommt: dieses hier, oder dieselbe Version fürs Ziel.
async function nodeFor() {
  if (target === HOST) return process.execPath;
  const version = process.version;
  const cache = join(dist, '.node', `${version}-${spec.node}`);
  const binary = join(cache, spec.os === 'win32' ? 'node.exe' : 'node');
  if (existsSync(binary)) return binary;
  mkdirSync(cache, { recursive: true });
  const base = `https://nodejs.org/dist/${version}`;
  if (spec.os === 'win32') {
    await download(`${base}/${spec.node}/node.exe`, binary);
  } else {
    const name = `node-${version}-${spec.node}`;
    const archive = join(cache, `${name}.tar.gz`);
    await download(`${base}/${name}.tar.gz`, archive);
    run('tar', ['-xzf', archive, '-C', cache, '--strip-components=2', `${name}/bin/node`]);
    rmSync(archive);
  }
  return binary;
}

async function download(url, to) {
  console.log(`↓ ${url}`);
  const res = await fetch(url);
  if (!res.ok) fail(`${url}: ${res.status}`);
  writeFileSync(to, Buffer.from(await res.arrayBuffer()));
}

rmSync(out, { force: true });
copyFileSync(await nodeFor(), out);
chmodSync(out, 0o755);

// 3. Einsetzen. Eine macOS-Signatur deckt die Datei ab; sie muss vorher weg und
//    danach neu (ad hoc — eine Developer-ID-Signatur gibt die Desktop-App, wenn
//    sie je eine bekommt, beim Bündeln mit).
if (spec.os === 'darwin') run('codesign', ['--remove-signature', out]);
run('npx', [
  '--yes', POSTJECT, out, 'NODE_SEA_BLOB', blob, '--sentinel-fuse', FUSE,
  ...(spec.os === 'darwin' ? ['--macho-segment-name', 'NODE_SEA'] : []),
]);
if (spec.os === 'darwin') run('codesign', ['--sign', '-', out]);

for (const f of [bundle, blob, config]) rmSync(f, { force: true });
console.log(`→ ${out}`);
