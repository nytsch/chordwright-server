/**
 * Eine eigene kleine Zertifizierungsstelle, nur mit node:crypto.
 *
 * Warum nicht einfach ein selbstsigniertes Zertifikat: Das muss man in jedem
 * Browser per Klick bestätigen, und eine App vom Home-Bildschirm (iPhone,
 * „Zum Dock hinzufügen" am Mac) hat keinen Ort für diesen Klick — ihre
 * Anfragen scheitern einfach. Eine CA dagegen installiert man einmal pro Gerät
 * als vertrauenswürdig, und danach ist jedes Zertifikat, das sie ausstellt,
 * überall gültig, ohne Warnung.
 *
 *  - `createCA()`: die Stelle selbst. Zehn Jahre, darf nur signieren.
 *  - `issueServerCert(ca, …)`: das Zertifikat, mit dem der Server spricht.
 *    825 Tage, SAN und serverAuth — darunter lehnen iOS und macOS ab.
 *
 * Node kann Schlüssel erzeugen und signieren, aber kein X.509 zusammensetzen —
 * das DER hier ist genau so viel davon, wie beides braucht.
 */

import { X509Certificate, createHash, createPrivateKey, createPublicKey, generateKeyPairSync, randomBytes, sign } from 'node:crypto';

export const CA_VALIDITY_DAYS = 3650;
export const SERVER_VALIDITY_DAYS = 825;
const DAY = 24 * 60 * 60 * 1000;

function length(n) {
  if (n < 0x80) return Buffer.from([n]);
  const bytes = [];
  for (let v = n; v > 0; v >>= 8) bytes.unshift(v & 0xff);
  return Buffer.from([0x80 | bytes.length, ...bytes]);
}

const tlv = (tag, ...parts) => {
  const body = Buffer.concat(parts);
  return Buffer.concat([Buffer.from([tag]), length(body.length), body]);
};
const seq = (...parts) => tlv(0x30, ...parts);
const set = (...parts) => tlv(0x31, ...parts);
const explicit = (n, ...parts) => tlv(0xa0 | n, ...parts);
const octets = (buf) => tlv(0x04, buf);
const bool = (v) => tlv(0x01, Buffer.from([v ? 0xff : 0]));
const bitString = (buf) => tlv(0x03, Buffer.from([0]), buf);

function oid(dotted) {
  const [a, b, ...rest] = dotted.split('.').map(Number);
  const bytes = [40 * a + b];
  for (const n of rest) {
    const chunk = [n & 0x7f];
    for (let v = n >> 7; v > 0; v >>= 7) chunk.unshift((v & 0x7f) | 0x80);
    bytes.push(...chunk);
  }
  return tlv(0x06, Buffer.from(bytes));
}

function integer(buf) {
  // DER integers are signed; a leading 1-bit would make it negative.
  return tlv(0x02, buf[0] & 0x80 ? Buffer.concat([Buffer.from([0]), buf]) : buf);
}

function utcTime(date) {
  const s = date.toISOString(); // 2026-09-23T17:00:00.000Z
  return tlv(0x17, Buffer.from(`${s.slice(2, 4)}${s.slice(5, 7)}${s.slice(8, 10)}${s.slice(11, 13)}${s.slice(14, 16)}${s.slice(17, 19)}Z`));
}

const name = (cn) => seq(set(seq(oid('2.5.4.3'), tlv(0x0c, Buffer.from(cn)))));
const extension = (id, critical, value) => seq(oid(id), ...(critical ? [bool(true)] : []), octets(value));
const ecdsaWithSha256 = () => seq(oid('1.2.840.10045.4.3.2'));

const pem = (label, der) =>
  `-----BEGIN ${label}-----\n${der.toString('base64').match(/.{1,64}/g).join('\n')}\n-----END ${label}-----\n`;

/** Key identifier: a hash of the public key — any stable, unique value will do. */
const keyId = (publicKey) => createHash('sha1').update(publicKey.export({ type: 'spki', format: 'der' })).digest();

function ipBytes(ip) {
  if (ip.includes(':')) {
    // IPv6, possibly with ::
    const [head, tail = ''] = ip.split('::');
    const h = head ? head.split(':') : [];
    const t = tail ? tail.split(':') : [];
    const groups = ip.includes('::') ? [...h, ...Array(8 - h.length - t.length).fill('0'), ...t] : h;
    return Buffer.from(groups.flatMap((g) => [(parseInt(g, 16) >> 8) & 0xff, parseInt(g, 16) & 0xff]));
  }
  return Buffer.from(ip.split('.').map(Number));
}

function certificate({ subject, issuer, publicKey, signingKey, notBefore, notAfter, extensions }) {
  const serial = randomBytes(16);
  serial[0] &= 0x7f;
  const tbs = seq(
    explicit(0, integer(Buffer.from([2]))), // v3
    integer(serial),
    ecdsaWithSha256(),
    name(issuer),
    seq(utcTime(notBefore), utcTime(notAfter)),
    name(subject),
    publicKey.export({ type: 'spki', format: 'der' }),
    explicit(3, seq(...extensions)),
  );
  const signature = sign('sha256', tbs, { key: signingKey, dsaEncoding: 'der' });
  return pem('CERTIFICATE', seq(tbs, ecdsaWithSha256(), bitString(signature)));
}

/**
 * @param {{ commonName?: string, now?: Date }} [options]
 * @returns {{ cert: string, key: string }} beides als PEM
 */
export function createCA({ commonName = 'Chordwright lokale CA', now = new Date() } = {}) {
  const { publicKey, privateKey } = generateKeyPairSync('ec', { namedCurve: 'prime256v1' });
  const cert = certificate({
    subject: commonName,
    issuer: commonName,
    publicKey,
    signingKey: privateKey,
    notBefore: new Date(now.getTime() - DAY),
    notAfter: new Date(now.getTime() + CA_VALIDITY_DAYS * DAY),
    extensions: [
      extension('2.5.29.19', true, seq(bool(true), integer(Buffer.from([0])))), // CA, pathLen 0
      extension('2.5.29.15', true, tlv(0x03, Buffer.from([0x01, 0x06]))), // keyCertSign, cRLSign
      extension('2.5.29.14', false, octets(keyId(publicKey))),
    ],
  });
  return { cert, key: privateKey.export({ type: 'pkcs8', format: 'pem' }) };
}

/**
 * @param {{ cert: string, key: string }} ca
 * @param {{ commonName?: string, dnsNames?: string[], ips?: string[], now?: Date }} [options]
 * @returns {{ cert: string, key: string }} beides als PEM
 */
export function issueServerCert(ca, { commonName = 'chordwright', dnsNames = [], ips = [], now = new Date() } = {}) {
  const caKey = createPrivateKey(ca.key);
  const caPublic = createPublicKey(ca.key);
  // Der Aussteller muss Byte für Byte dem Namen der CA entsprechen; beide entstehen aus `name()`.
  const caName = /CN=([^\n]+)/.exec(new X509Certificate(ca.cert).subject)?.[1] ?? 'Chordwright lokale CA';
  const { publicKey, privateKey } = generateKeyPairSync('ec', { namedCurve: 'prime256v1' });
  const altNames = seq(
    ...dnsNames.map((d) => tlv(0x82, Buffer.from(d))),
    ...ips.map((ip) => tlv(0x87, ipBytes(ip))),
  );
  const cert = certificate({
    subject: commonName,
    issuer: caName,
    publicKey,
    signingKey: caKey,
    // Uhren gehen nicht überall gleich.
    notBefore: new Date(now.getTime() - 60 * 60 * 1000),
    notAfter: new Date(now.getTime() + SERVER_VALIDITY_DAYS * DAY),
    extensions: [
      extension('2.5.29.19', true, seq()), // kein CA
      extension('2.5.29.15', true, tlv(0x03, Buffer.from([0x07, 0x80]))), // digitalSignature
      extension('2.5.29.37', false, seq(oid('1.3.6.1.5.5.7.3.1'))), // serverAuth
      extension('2.5.29.17', false, altNames),
      extension('2.5.29.14', false, octets(keyId(publicKey))),
      extension('2.5.29.35', false, seq(tlv(0x80, keyId(caPublic)))), // authorityKeyIdentifier
    ],
  });
  return { cert, key: privateKey.export({ type: 'pkcs8', format: 'pem' }) };
}
