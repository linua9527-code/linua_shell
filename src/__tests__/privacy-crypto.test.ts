import { describe, expect, it } from 'vitest';
import { decryptText, encryptText } from '@/lib/privacy-crypto';

describe('privacy crypto', () => {
  it('round-trips unicode plaintext with AES-GCM', async () => {
    const plaintext = '隐私文本\nR-Shell / AES-GCM';
    const ciphertext = await encryptText(plaintext, 'test-key');

    expect(ciphertext.startsWith('RSH-AES1.')).toBe(true);
    await expect(decryptText(ciphertext, 'test-key')).resolves.toBe(plaintext);
  });

  it('uses a fresh randomized envelope for each encryption', async () => {
    const first = await encryptText('same input', 'same key');
    const second = await encryptText('same input', 'same key');

    expect(second).not.toBe(first);
  });

  it('supports an empty optional key', async () => {
    const plaintext = 'empty key support';
    const ciphertext = await encryptText(plaintext, '');

    await expect(decryptText(ciphertext, '')).resolves.toBe(plaintext);
    await expect(decryptText(ciphertext, 'different-key')).rejects.toThrow();
  });

  it('rejects a ciphertext with the wrong key', async () => {
    const ciphertext = await encryptText('protected', 'correct-key');

    await expect(decryptText(ciphertext, 'wrong-key')).rejects.toThrow();
  });
});
