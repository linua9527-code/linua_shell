import { invoke } from '@tauri-apps/api/core';
import { isTauriRuntime } from './privacy';

const SENSITIVE_KEYS = [
  'r-shell-connections',
  'r-shell-connection-folders',
  'r-shell-connection-profiles',
  'r-shell-terminal-groups',
] as const;

const records = new Map<string, string>();
let ready = false;
let revision = 0;
let savedRevision = 0;
let writeQueue: Promise<void> = Promise.resolve();

/** @brief 按顺序保存内存快照；失败时保留待保存状态，禁止回退到明文。 */
export function flushEncryptedStorage(): Promise<void> {
  if (!ready) return Promise.reject(new Error('Encrypted storage not initialized'));
  const targetRevision = revision;
  const snapshot = Object.fromEntries(records);
  const operation = writeQueue.then(async () => {
    if (targetRevision <= savedRevision) return;
    if (isTauriRuntime()) await invoke('secure_store_save', { records: snapshot });
    savedRevision = targetRevision;
  });
  writeQueue = operation.catch((error: unknown) => {
    window.dispatchEvent(new CustomEvent('encrypted-storage-error', { detail: error }));
  });
  return operation;
}

/** @brief 在应用解锁后加载加密数据，旧明文仅在成功落盘后删除。 */
export async function initializeEncryptedStorage(): Promise<void> {
  if (ready) return;
  records.clear();
  const stored = isTauriRuntime()
    ? await invoke<Record<string, string>>('secure_store_load')
    : {};
  for (const [key, value] of Object.entries(stored)) {
    if (typeof value !== 'string') throw new Error('Invalid encrypted storage record');
    JSON.parse(value);
    records.set(key, value);
  }

  const plaintextKeys: string[] = [];
  if (isTauriRuntime()) {
    for (const key of SENSITIVE_KEYS) {
      const value = localStorage.getItem(key);
      if (value === null) continue;
      JSON.parse(value);
      plaintextKeys.push(key);
      if (!records.has(key)) records.set(key, value);
    }

    const legacySessions = localStorage.getItem('r-shell-sessions');
    const legacyFolders = localStorage.getItem('r-shell-session-folders');
    if (legacySessions !== null && !records.has('r-shell-connections')) {
      const sessions: unknown = JSON.parse(legacySessions);
      if (!Array.isArray(sessions)) throw new Error('Invalid legacy connection records');
      const migrated = sessions.map((session: unknown) => {
        if (!session || typeof session !== 'object') throw new Error('Invalid legacy connection record');
        const record = session as Record<string, unknown>;
        return {
          ...record,
          folder: typeof record.folder === 'string'
            ? record.folder.replace(/All Sessions/g, 'All Connections')
            : record.folder,
        };
      });
      records.set('r-shell-connections', JSON.stringify(migrated));
      plaintextKeys.push('r-shell-sessions');
    }
    if (legacyFolders !== null && !records.has('r-shell-connection-folders')) {
      const folders: unknown = JSON.parse(legacyFolders);
      if (!Array.isArray(folders)) throw new Error('Invalid legacy connection folders');
      const migrated = folders.map((folder: unknown) => {
        if (!folder || typeof folder !== 'object') throw new Error('Invalid legacy connection folder');
        const record = folder as Record<string, unknown>;
        return Object.fromEntries(Object.entries(record).map(([key, value]) => [
          key,
          ['name', 'path', 'parentPath'].includes(key) && typeof value === 'string'
            ? value.replace(/All Sessions/g, 'All Connections')
            : value,
        ]));
      });
      records.set('r-shell-connection-folders', JSON.stringify(migrated));
      plaintextKeys.push('r-shell-session-folders');
    }
  }
  ready = true;
  if (plaintextKeys.length > 0) {
    revision++;
    try {
      await flushEncryptedStorage();
      for (const key of plaintextKeys) localStorage.removeItem(key);
    } catch (error) {
      ready = false;
      throw error;
    }
  }
  if (isTauriRuntime()) {
    localStorage.removeItem('r-shell-sessions');
    localStorage.removeItem('r-shell-session-folders');
    localStorage.removeItem('r-shell-active-connections');
  }
}

/** @brief 提供同步内存访问，所有持久化写入统一经过后端认证加密。 */
export const encryptedStorage = {
  getItem(key: string): string | null {
    return records.get(key) ?? null;
  },
  setItem(key: string, value: string): void {
    if (!ready) throw new Error('Encrypted storage not initialized');
    records.set(key, value);
    revision++;
    void flushEncryptedStorage().catch(() => {});
  },
  removeItem(key: string): void {
    if (!ready) throw new Error('Encrypted storage not initialized');
    if (!records.delete(key)) return;
    revision++;
    void flushEncryptedStorage().catch(() => {});
  },
};
