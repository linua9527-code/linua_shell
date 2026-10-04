const FORMAT_PREFIX = 'RSH-AES1';
const KEY_LENGTH = 256;
const SALT_LENGTH = 16;
const IV_LENGTH = 12;
const MIN_ITERATIONS = 100_000;
const MAX_ITERATIONS = 1_000_000;
const PBKDF2_ITERATIONS = 250_000;
// WebKitGTK rejects a zero-length PBKDF2 key. Keep the UI's optional-key
// behavior by deriving empty-key ciphertexts from a stable one-byte marker.
const EMPTY_KEY_DERIVATION_INPUT = new Uint8Array([0]);

interface CipherEnvelope {
  version: 1;
  algorithm: 'AES-GCM';
  iterations: number;
  salt: string;
  iv: string;
  data: string;
}

function getWebCrypto(): Crypto {
  if (!globalThis.crypto?.subtle) {
    throw new Error('Web Crypto API is unavailable');
  }
  return globalThis.crypto;
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = '';
  const chunkSize = 0x8000;

  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    const chunk = bytes.subarray(offset, Math.min(offset + chunkSize, bytes.length));
    binary += String.fromCharCode(...chunk);
  }

  return btoa(binary);
}

function base64ToBytes(value: string): Uint8Array {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);

  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }

  return bytes;
}

function decodeUtf8(bytes: Uint8Array): string {
  return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
}

function asArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  const copy = new Uint8Array(bytes.byteLength);
  copy.set(bytes);
  return copy.buffer;
}

async function deriveKey(keyText: string, salt: Uint8Array, iterations: number): Promise<CryptoKey> {
  const cryptoApi = getWebCrypto();
  const keyBytes = new TextEncoder().encode(keyText);
  const material = await cryptoApi.subtle.importKey(
    'raw',
    keyBytes.length > 0 ? keyBytes : EMPTY_KEY_DERIVATION_INPUT,
    'PBKDF2',
    false,
    ['deriveKey'],
  );

  return cryptoApi.subtle.deriveKey(
    {
      name: 'PBKDF2',
      salt: asArrayBuffer(salt),
      iterations,
      hash: 'SHA-256',
    },
    material,
    { name: 'AES-GCM', length: KEY_LENGTH },
    false,
    ['encrypt', 'decrypt'],
  );
}

function encodeEnvelope(envelope: CipherEnvelope): string {
  const json = JSON.stringify(envelope);
  return `${FORMAT_PREFIX}.${bytesToBase64(new TextEncoder().encode(json))}`;
}

function decodeEnvelope(value: string): CipherEnvelope {
  const [prefix, payload, ...extra] = value.trim().split('.');
  if (prefix !== FORMAT_PREFIX || !payload || extra.length > 0) {
    throw new Error('Unsupported ciphertext format');
  }

  let envelope: unknown;
  try {
    envelope = JSON.parse(decodeUtf8(base64ToBytes(payload)));
  } catch {
    throw new Error('Invalid ciphertext');
  }

  if (!envelope || typeof envelope !== 'object') {
    throw new Error('Invalid ciphertext');
  }

  const candidate = envelope as Partial<CipherEnvelope>;
  if (
    candidate.version !== 1 ||
    candidate.algorithm !== 'AES-GCM' ||
    typeof candidate.iterations !== 'number' ||
    !Number.isInteger(candidate.iterations) ||
    candidate.iterations < MIN_ITERATIONS ||
    candidate.iterations > MAX_ITERATIONS ||
    typeof candidate.salt !== 'string' ||
    typeof candidate.iv !== 'string' ||
    typeof candidate.data !== 'string'
  ) {
    throw new Error('Invalid ciphertext');
  }

  const salt = base64ToBytes(candidate.salt);
  const iv = base64ToBytes(candidate.iv);
  if (salt.length !== SALT_LENGTH || iv.length !== IV_LENGTH) {
    throw new Error('Invalid ciphertext');
  }

  return candidate as CipherEnvelope;
}

export async function encryptText(plaintext: string, keyText: string): Promise<string> {
  const cryptoApi = getWebCrypto();
  const salt = cryptoApi.getRandomValues(new Uint8Array(SALT_LENGTH));
  const iv = cryptoApi.getRandomValues(new Uint8Array(IV_LENGTH));
  const key = await deriveKey(keyText, salt, PBKDF2_ITERATIONS);
  const encrypted = await cryptoApi.subtle.encrypt(
    { name: 'AES-GCM', iv: asArrayBuffer(iv) },
    key,
    new TextEncoder().encode(plaintext),
  );

  return encodeEnvelope({
    version: 1,
    algorithm: 'AES-GCM',
    iterations: PBKDF2_ITERATIONS,
    salt: bytesToBase64(salt),
    iv: bytesToBase64(iv),
    data: bytesToBase64(new Uint8Array(encrypted)),
  });
}

export async function decryptText(ciphertext: string, keyText: string): Promise<string> {
  const cryptoApi = getWebCrypto();
  const envelope = decodeEnvelope(ciphertext);
  const salt = base64ToBytes(envelope.salt);
  const iv = base64ToBytes(envelope.iv);
  const encrypted = base64ToBytes(envelope.data);
  const key = await deriveKey(keyText, salt, envelope.iterations);
  const decrypted = await cryptoApi.subtle.decrypt(
    { name: 'AES-GCM', iv: asArrayBuffer(iv) },
    key,
    asArrayBuffer(encrypted),
  );

  try {
    return decodeUtf8(new Uint8Array(decrypted));
  } catch {
    throw new Error('Invalid plaintext');
  }
}
