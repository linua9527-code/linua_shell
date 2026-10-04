import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.unmock('@/lib/encrypted-storage');

const { invoke, isTauriRuntime } = vi.hoisted(() => ({
  invoke: vi.fn(),
  isTauriRuntime: vi.fn(() => true),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('@/lib/privacy', () => ({ isTauriRuntime }));

beforeEach(() => {
  vi.resetModules();
  localStorage.clear();
  invoke.mockReset();
  isTauriRuntime.mockReturnValue(true);
  invoke.mockImplementation(async (command: string) => command === 'secure_store_load' ? {} : undefined);
});

afterEach(() => vi.restoreAllMocks());

describe('encrypted connection storage', () => {
  it('loads sensitive data into memory without writing plaintext to localStorage', async () => {
    const value = JSON.stringify([{ host: 'private.example.com', password: 'secret' }]);
    invoke.mockResolvedValueOnce({ 'r-shell-connections': value });
    const storage = await import('@/lib/encrypted-storage');
    await storage.initializeEncryptedStorage();

    expect(storage.encryptedStorage.getItem('r-shell-connections')).toBe(value);
    expect(localStorage.length).toBe(0);
  });

  it('removes obsolete plaintext session keys after successful initialization', async () => {
    localStorage.setItem('r-shell-active-connections', '[{"password":"old"}]');
    localStorage.setItem('r-shell-sessions', '[{"folder":"All Sessions/Work","password":"old"}]');
    localStorage.setItem('r-shell-session-folders', '[{"name":"All Sessions","path":"All Sessions"}]');
    const storage = await import('@/lib/encrypted-storage');
    await storage.initializeEncryptedStorage();

    expect(localStorage.length).toBe(0);
    expect(JSON.parse(storage.encryptedStorage.getItem('r-shell-connections') ?? '[]')).toEqual([
      { folder: 'All Connections/Work', password: 'old' },
    ]);
    expect(JSON.parse(storage.encryptedStorage.getItem('r-shell-connection-folders') ?? '[]')).toEqual([
      { name: 'All Connections', path: 'All Connections' },
    ]);
  });

  it('retains older records when encryption fails during migration', async () => {
    localStorage.setItem('r-shell-sessions', '[{"password":"old"}]');
    invoke.mockImplementation(async (command: string) => {
      if (command === 'secure_store_load') return {};
      throw new Error('Disk full');
    });
    const storage = await import('@/lib/encrypted-storage');

    await expect(storage.initializeEncryptedStorage()).rejects.toThrow('Disk full');
    expect(localStorage.getItem('r-shell-sessions')).toContain('old');
  });

  it('encrypts all existing records before removing plaintext, including workspace metadata', async () => {
    const keys = ['r-shell-connections', 'r-shell-connection-folders', 'r-shell-connection-profiles', 'r-shell-terminal-groups'];
    for (const key of keys) localStorage.setItem(key, JSON.stringify({ password: 'secret' }));
    const saved = Promise.withResolvers<void>();
    invoke.mockImplementation((command: string) => command === 'secure_store_load' ? Promise.resolve({}) : saved.promise);
    const storage = await import('@/lib/encrypted-storage');
    const loading = storage.initializeEncryptedStorage();

    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('secure_store_save', {
      records: Object.fromEntries(keys.map((key) => [key, JSON.stringify({ password: 'secret' })])),
    }));
    for (const key of keys) expect(localStorage.getItem(key)).not.toBeNull();
    saved.resolve();
    await loading;
    for (const key of keys) expect(localStorage.getItem(key)).toBeNull();
  });

  it('retains existing plaintext if the encrypted write fails and rejects initialization', async () => {
    localStorage.setItem('r-shell-connections', '[]');
    invoke.mockImplementation(async (command: string) => {
      if (command === 'secure_store_load') return {};
      throw new Error('Disk full');
    });
    const storage = await import('@/lib/encrypted-storage');

    await expect(storage.initializeEncryptedStorage()).rejects.toThrow('Disk full');
    expect(localStorage.getItem('r-shell-connections')).toBe('[]');
    expect(() => storage.encryptedStorage.setItem('r-shell-connections', '[]')).toThrow('not initialized');
  });

  it('reports a write error and retries without falling back to plaintext', async () => {
    const storage = await import('@/lib/encrypted-storage');
    await storage.initializeEncryptedStorage();
    const errors = vi.fn();
    window.addEventListener('encrypted-storage-error', errors);
    invoke.mockRejectedValueOnce(new Error('Write failed'));
    storage.encryptedStorage.setItem('r-shell-connections', '[{"password":"secret"}]');

    await vi.waitFor(() => expect(errors).toHaveBeenCalled());
    expect(localStorage.getItem('r-shell-connections')).toBeNull();
    await storage.flushEncryptedStorage();
    expect(invoke).toHaveBeenLastCalledWith('secure_store_save', {
      records: { 'r-shell-connections': '[{"password":"secret"}]' },
    });
    window.removeEventListener('encrypted-storage-error', errors);
  });

  it('persists concurrent mutations in order and includes the final snapshot', async () => {
    const storage = await import('@/lib/encrypted-storage');
    await storage.initializeEncryptedStorage();
    storage.encryptedStorage.setItem('r-shell-connections', '[1]');
    storage.encryptedStorage.setItem('r-shell-connection-folders', '[2]');
    storage.encryptedStorage.setItem('r-shell-connections', '[3]');
    await storage.flushEncryptedStorage();

    expect(invoke).toHaveBeenLastCalledWith('secure_store_save', {
      records: { 'r-shell-connections': '[3]', 'r-shell-connection-folders': '[2]' },
    });
    expect(localStorage.length).toBe(0);
  });

  it('rejects corrupt encrypted storage instead of overwriting it with defaults', async () => {
    invoke.mockRejectedValueOnce(new Error('Storage authentication failed'));
    const storage = await import('@/lib/encrypted-storage');
    await expect(storage.initializeEncryptedStorage()).rejects.toThrow('authentication failed');
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it('uses only volatile memory in browser preview', async () => {
    isTauriRuntime.mockReturnValue(false);
    const storage = await import('@/lib/encrypted-storage');
    await storage.initializeEncryptedStorage();
    storage.encryptedStorage.setItem('r-shell-connections', '[{"password":"secret"}]');
    await storage.flushEncryptedStorage();

    expect(invoke).not.toHaveBeenCalled();
    expect(localStorage.length).toBe(0);
  });
});
