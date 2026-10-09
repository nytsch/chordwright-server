/**
 * Baut den Server mit cargo und lässt test/server.test.mjs gegen das Programm
 * laufen. Mit gesetztem CHORDWRIGHT_SERVER_BIN wird nichts gebaut, sondern
 * dieses Programm geprüft — so prüft die CI jede Datei, die an ein Release
 * kommt.
 *
 *   npm test
 */

import { execFileSync, spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
let bin = process.env.CHORDWRIGHT_SERVER_BIN;
if (!bin) {
  execFileSync('cargo', ['build', '--bin', 'chordwright-server'], { cwd: root, stdio: 'inherit' });
  const target = process.env.CARGO_TARGET_DIR ? resolve(process.env.CARGO_TARGET_DIR) : join(root, 'target');
  bin = join(target, 'debug', `chordwright-server${process.platform === 'win32' ? '.exe' : ''}`);
}
const run = spawnSync(process.execPath, ['--test', '--test-timeout=60000', join('test', 'server.test.mjs')], {
  cwd: root,
  env: { ...process.env, CHORDWRIGHT_SERVER_BIN: resolve(bin) },
  stdio: 'inherit',
});
process.exit(run.status ?? 1);
