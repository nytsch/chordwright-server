/**
 * Ein selbstsigniertes TLS-Zertifikat, nur mit node:crypto.
 *
 * Node kann Schlüssel erzeugen und signieren, aber kein X.509 zusammensetzen —
 * das DER hier ist genau so viel davon, wie ein Serverzertifikat braucht. So
 * muss das Image kein openssl nachinstallieren und baut ohne Netz.
 *
 * Laufzeit 825 Tage und die Erweiterungen SAN + serverAuth: darunter lehnen
 * iOS und macOS ein Zertifikat ab, auch ein bewusst bestätigtes.
 */

import { generateKeyPairSync, randomBytes, sign } from 'node:crypto';

export const VALIDITY_DAYS = 825;

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

function ipBytes(ip) {
  return Buffer.from(ip.split('.').map(Number));
}

const extension = (id, critical, value) => seq(oid(id), ...(critical ? [bool(true)] : []), octets(value));

const pem = (label, der) =>
  `-----BEGIN ${label}-----\n${der.toString('base64').match(/.{1,64}/g).join('\n')}\n-----END ${label}-----\n`;

/**
 * @param {{ commonName?: string, dnsNames?: string[], ips?: string[], now?: Date }} [options]
 * @returns {{ cert: string, key: string }} beides als PEM
 */
export function createSelfSigned({
  commonName = 'chordwright',
  dnsNames = ['homeassistant.local', 'homeassistant', 'localhost'],
  ips = ['127.0.0.1'],
  now = new Date(),
} = {}) {
  const { publicKey, privateKey } = generateKeyPairSync('ec', { namedCurve: 'prime256v1' });
  const ecdsaWithSha256 = seq(oid('1.2.840.10045.4.3.2'));

  const serial = randomBytes(16);
  serial[0] &= 0x7f;
  const notBefore = new Date(now.getTime() - 60 * 60 * 1000); // Uhren gehen nicht überall gleich
  const notAfter = new Date(now.getTime() + VALIDITY_DAYS * 24 * 60 * 60 * 1000);

  const altNames = seq(
    ...dnsNames.map((d) => tlv(0x82, Buffer.from(d))),
    ...ips.map((ip) => tlv(0x87, ipBytes(ip))),
  );

  const tbs = seq(
    explicit(0, integer(Buffer.from([2]))), // v3
    integer(serial),
    ecdsaWithSha256,
    name(commonName),
    seq(utcTime(notBefore), utcTime(notAfter)),
    name(commonName),
    publicKey.export({ type: 'spki', format: 'der' }),
    explicit(3, seq(
      extension('2.5.29.19', true, seq()), // basicConstraints: kein CA
      extension('2.5.29.15', true, tlv(0x03, Buffer.from([0x07, 0x80]))), // keyUsage: digitalSignature
      extension('2.5.29.37', false, seq(oid('1.3.6.1.5.5.7.3.1'))), // extKeyUsage: serverAuth
      extension('2.5.29.17', false, altNames),
    )),
  );

  const signature = sign('sha256', tbs, { key: privateKey, dsaEncoding: 'der' });
  const certificate = seq(tbs, ecdsaWithSha256, tlv(0x03, Buffer.from([0]), signature));

  return {
    cert: pem('CERTIFICATE', certificate),
    key: privateKey.export({ type: 'pkcs8', format: 'pem' }),
  };
}
