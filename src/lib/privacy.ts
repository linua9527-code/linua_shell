import { invoke } from '@tauri-apps/api/core';

interface TauriWindow extends Window {
  __TAURI_INTERNALS__?: unknown;
}

export interface PrivacyStatus {
  configured: boolean;
}

/** @brief 判断当前页面是否由 Tauri 桌面运行时承载。 */
export function isTauriRuntime(): boolean {
  if (typeof window === 'undefined') return false;
  return Boolean((window as TauriWindow).__TAURI_INTERNALS__);
}

/** @brief 将 Tauri 命令的未知错误转换为可读文本。 */
export function getPrivacyErrorMessage(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === 'string' && error) return error;

  try {
    const serialized = JSON.stringify(error);
    return serialized && serialized !== '{}' ? serialized : 'Unknown privacy error';
  } catch {
    return 'Unknown privacy error';
  }
}

export async function getPrivacyStatus(): Promise<PrivacyStatus> {
  return invoke<PrivacyStatus>('privacy_get_status');
}

export async function verifyPrivacyPhrase(phrase: string): Promise<boolean> {
  return invoke<boolean>('privacy_verify_phrase', { phrase });
}
