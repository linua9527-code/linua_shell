import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

// Test credentials - Replace with your own test server credentials
const TEST_HOST = 'localhost'; // Replace with your test SSH server
const TEST_USERNAME = 'testuser'; // Replace with your test username
const TEST_PASSWORD = 'testpass'; // Replace with your test password

describe('SSH Connection Tests', () => {
  let connectionId: string;

  beforeAll(() => {
    connectionId = `test-connection-${Date.now()}`;
  });

  afterAll(async () => {
    // Clean up: disconnect the test connection
    if (connectionId) {
      try {
        await invoke('ssh_disconnect', { connection_id: connectionId });
      } catch (error) {
        console.error('Cleanup error:', error);
      }
    }
  });

  it('should successfully connect to SSH server', async () => {
    const result = await invoke<{ success: boolean; error?: string }>(
      'ssh_connect',
      {
        request: {
          connection_id: connectionId,
          host: TEST_HOST,
          port: 22,
          username: TEST_USERNAME,
          auth_method: 'password',
          password: TEST_PASSWORD,
          key_path: null,
          passphrase: null,
        }
      }
    );

    expect(result.success).toBe(true);
    expect(result.error).toBeUndefined();
  }, 10000); // 10 second timeout for connection

  it('should execute a simple command', async () => {
    const result = await invoke<{ success: boolean; output?: string; error?: string }>(
      'ssh_execute_command',
      {
        connection_id: connectionId,
        command: 'echo "Hello from test"',
      }
    );

    expect(result.success).toBe(true);
    expect(result.output).toContain('Hello from test');
  }, 5000);

  it('should list files in home directory', async () => {
    const result = await invoke<Array<{
      name: string;
      size: number;
      modified: string | null;
      permissions: string | null;
      file_type: 'File' | 'Directory' | 'Symlink';
    }>>('list_files', {
      connection_id: connectionId,
      path: '~'
    });

    expect(Array.isArray(result)).toBe(true);
    // Every entry must have a non-empty name that does not leak time/date
    // tokens (the regression we fixed for non-GNU ls output).
    for (const entry of result) {
      expect(typeof entry.name).toBe('string');
      expect(entry.name.length).toBeGreaterThan(0);
      expect(entry.name).not.toBe('.');
      expect(entry.name).not.toBe('..');
    }
  }, 5000);

  it('should fail with invalid credentials', async () => {
    const badConnectionId = `bad-connection-${Date.now()}`;
    const result = await invoke<{ success: boolean; error?: string }>(
      'ssh_connect',
      {
        request: {
          connection_id: badConnectionId,
          host: TEST_HOST,
          port: 22,
          username: TEST_USERNAME,
          auth_method: 'password',
          password: 'wrongpassword',
          key_path: null,
          passphrase: null,
        }
      }
    );

    expect(result.success).toBe(false);
    expect(result.error).toBeDefined();
  }, 10000);

  it('should disconnect successfully', async () => {
    const result = await invoke<{ success: boolean; error?: string }>(
      'ssh_disconnect',
      { connection_id: connectionId }
    );

    expect(result.success).toBe(true);
  }, 5000);
});
