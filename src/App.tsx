import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { applyLanguageFromPreference } from './lib/i18n';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { MenuBar } from './components/menu-bar';
import { ConnectionManager } from './components/connection-manager';
import { StatusBar } from './components/status-bar';
import { ConnectionDialog, ConnectionConfig } from './components/connection-dialog';
import { SettingsModal } from './components/settings-modal';
import { IntegratedFileBrowser } from './components/integrated-file-browser';
import { WelcomeScreen } from './components/welcome-screen';
import { UpdateChecker } from './components/update-checker';
import { toConnectionConfig } from './lib/connection-config';
import { ConnectionStorageManager } from './lib/connection-storage';
import { isDesktopProtocol } from './lib/protocol-config';
import { buildSshConnectRequest } from './lib/ssh-connect-request';
import { useProxyConnectionConfirmation } from './components/proxy-connection-confirmation';
import { useLayout, LayoutProvider } from './lib/layout-context';
import {
  APP_SETTINGS_CHANGED_EVENT,
  createLayoutShortcuts,
  createSplitViewShortcuts,
  loadKeyboardShortcutSettings,
  useKeyboardShortcuts,
} from './lib/keyboard-shortcuts';
import type { SplitViewShortcutBindings } from './lib/keyboard-shortcuts';
import { TerminalGroupProvider, useTerminalGroups } from './lib/terminal-group-context';
import { TerminalCallbacksProvider } from './lib/terminal-callbacks-context';
import { GridRenderer } from './components/terminal/grid-renderer';
import { ErrorBoundary } from './components/error-boundary';
import type { TerminalTab } from './lib/terminal-group-types';
import { Toaster } from './components/ui/sonner';
import { toast } from 'sonner';
import { dispatchTerminalCommand, type TerminalCommand } from './lib/terminal-commands';
import { getPrivacyErrorMessage, getPrivacyStatus, isTauriRuntime } from './lib/privacy';
import { PrivacyCryptoPage } from './components/privacy-crypto-page';
import { flushEncryptedStorage, initializeEncryptedStorage } from './lib/encrypted-storage';

import { ResizableHandle, ResizablePanel, ResizablePanelGroup } from './components/ui/resizable';

interface ConnectionNode {
  id: string;
  name: string;
  type: 'folder' | 'connection';
  path?: string;
  protocol?: string;
  host?: string;
  port?: number;
  username?: string;
  isConnected?: boolean;
  children?: ConnectionNode[];
  isExpanded?: boolean;
}

function AppContent() {
  const { t } = useTranslation();
  const { confirmConnection, confirmationDialog } = useProxyConnectionConfirmation();

  useEffect(() => {
    const reportStorageError = (event: Event) => {
      toast.error(t('app.storageSaveFailed'), {
        description: getPrivacyErrorMessage((event as CustomEvent<unknown>).detail),
      });
    };
    window.addEventListener('encrypted-storage-error', reportStorageError);
    return () => window.removeEventListener('encrypted-storage-error', reportStorageError);
  }, [t]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void import('@tauri-apps/api/window').then(async ({ getCurrentWindow }) => {
      const currentWindow = getCurrentWindow();
      const stop = await currentWindow.onCloseRequested(async (event) => {
        event.preventDefault();
        try {
          await flushEncryptedStorage();
          await currentWindow.destroy();
        } catch (error) {
          toast.error(t('app.storageSaveFailed'), { description: getPrivacyErrorMessage(error) });
        }
      });
      if (disposed) stop();
      else unlisten = stop;
    }).catch((error: unknown) => {
      toast.error(t('app.storageSaveFailed'), { description: getPrivacyErrorMessage(error) });
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [t]);
  const [selectedConnection, setSelectedConnection] = useState<ConnectionNode | null>(null);

  // Terminal group state from context
  const { state, dispatch, activeGroup, activeTab, activeConnection } = useTerminalGroups();
  const workingDirectorySequenceRef = useRef(0);
  const [terminalWorkingDirectories, setTerminalWorkingDirectories] = useState<
    Record<string, { path: string; sequence: number }>
  >({});

  const handleWorkingDirectoryChange = useCallback((connectionId: string, path: string) => {
    setTerminalWorkingDirectories((previous) => ({
      ...previous,
      [connectionId]: {
        path,
        sequence: ++workingDirectorySequenceRef.current,
      },
    }));
  }, []);

  // Modal states
  const [connectionDialogOpen, setConnectionDialogOpen] = useState(false);
  const [connectionInitialFolder, setConnectionInitialFolder] = useState<string | undefined>();
  const [settingsModalOpen, setSettingsModalOpen] = useState(false);
  const [editingConnection, setEditingConnection] = useState<ConnectionConfig | null>(null);
  // Incremented after any save/connect dialog close to trigger sidebar refresh
  const [connectionSaveTrigger, setConnectionSaveTrigger] = useState(0);
  const [updateCheckSignal, setUpdateCheckSignal] = useState(0);
  const [keyboardShortcutSettings, setKeyboardShortcutSettings] = useState<SplitViewShortcutBindings>(
    () => loadKeyboardShortcutSettings(),
  );

  // Layout management
  const {
    layout,
    toggleLeftSidebar,
    toggleBottomPanel,
    toggleZenMode,
    setLeftSidebarSize,
    setBottomPanelSize,
    applyPreset,
  } = useLayout();

  // Collect all tabs across all groups for compatibility with existing features
  const allTabs = useMemo(() => {
    return Object.values(state.groups).flatMap(g => g.tabs);
  }, [state.groups]);

  // Memoized set of active connection IDs — stable reference prevents
  // ConnectionManager from rebuilding its tree on every parent render.
  const activeConnectionIds = useMemo(
    () => new Set(allTabs.map(tab => tab.id)),
    [allTabs],
  );

  const activeTerminalId = activeTab
    && (activeTab.tabType === undefined || activeTab.tabType === 'terminal')
    && activeTab.connectionStatus !== 'pending'
    ? activeTab.id
    : null;

  const runActiveTerminalCommand = useCallback((command: TerminalCommand) => {
    if (activeTerminalId) {
      dispatchTerminalCommand(activeTerminalId, command);
    }
  }, [activeTerminalId]);

  // Apply stored language preference (follows OS locale when set to "auto")
  useEffect(() => {
    void applyLanguageFromPreference();
  }, []);

  useEffect(() => {
    const refreshKeyboardShortcutSettings = () => {
      setKeyboardShortcutSettings(loadKeyboardShortcutSettings());
    };

    window.addEventListener(APP_SETTINGS_CHANGED_EVENT, refreshKeyboardShortcutSettings);
    window.addEventListener('storage', refreshKeyboardShortcutSettings);
    return () => {
      window.removeEventListener(APP_SETTINGS_CHANGED_EVENT, refreshKeyboardShortcutSettings);
      window.removeEventListener('storage', refreshKeyboardShortcutSettings);
    };
  }, []);

  const handleCloseActiveTab = useCallback(() => {
    if (!activeGroup?.activeTabId) {
      return;
    }

    dispatch({ type: 'REMOVE_TAB', groupId: activeGroup.id, tabId: activeGroup.activeTabId });
  }, [activeGroup, allTabs.length, dispatch]);

  // Keyboard shortcuts: layout + split view
  const splitViewShortcuts = useMemo(() => {
    const groupIds = Object.keys(state.groups);
    return createSplitViewShortcuts(
      {
        splitRight: () => {
          if (state.activeGroupId) {
            dispatch({ type: 'SPLIT_GROUP', groupId: state.activeGroupId, direction: 'right' });
          }
        },
        splitDown: () => {
          if (state.activeGroupId) {
            dispatch({ type: 'SPLIT_GROUP', groupId: state.activeGroupId, direction: 'down' });
          }
        },
        focusGroup: (index: number) => {
          if (index < groupIds.length) {
            dispatch({ type: 'ACTIVATE_GROUP', groupId: groupIds[index] });
          }
        },
        closeTab: () => {
          handleCloseActiveTab();
        },
        nextTab: () => {
          if (activeGroup && activeGroup.activeTabId && activeGroup.tabs.length > 1) {
            const currentIndex = activeGroup.tabs.findIndex(t => t.id === activeGroup.activeTabId);
            const nextIndex = (currentIndex + 1) % activeGroup.tabs.length;
            dispatch({ type: 'ACTIVATE_TAB', groupId: activeGroup.id, tabId: activeGroup.tabs[nextIndex].id });
          }
        },
        prevTab: () => {
          if (activeGroup && activeGroup.activeTabId && activeGroup.tabs.length > 1) {
            const currentIndex = activeGroup.tabs.findIndex(t => t.id === activeGroup.activeTabId);
            const prevIndex = (currentIndex - 1 + activeGroup.tabs.length) % activeGroup.tabs.length;
            dispatch({ type: 'ACTIVATE_TAB', groupId: activeGroup.id, tabId: activeGroup.tabs[prevIndex].id });
          }
        },
      },
      keyboardShortcutSettings,
    );
  }, [state.activeGroupId, state.groups, activeGroup, dispatch, handleCloseActiveTab, keyboardShortcutSettings]);

  const layoutShortcuts = useMemo(() => createLayoutShortcuts({
    toggleLeftSidebar,
    toggleBottomPanel,
    toggleZenMode,
  }), [toggleLeftSidebar, toggleBottomPanel, toggleZenMode]);

  useKeyboardShortcuts([...layoutShortcuts, ...splitViewShortcuts], true);

  const handleConnectionSelect = (connection: ConnectionNode) => {
    setSelectedConnection(connection);
  };

  const handleConnectionConnect = async (connection: ConnectionNode) => {
    if (connection.type === 'connection') {
      setSelectedConnection(connection);

      // Always use a unique session ID (see sessionId below) to prevent the backend
      // from reusing a stale session from a previously closed tab that was never
      // disconnected. This guarantees a fresh TCP connection with the latest config.
      const connectionData = ConnectionStorageManager.getConnection(connection.id);
      if (!connectionData) return;

      const isSftp = connectionData.protocol === 'SFTP';
      const isFtp = connectionData.protocol === 'FTP';
      const isFileBrowser = isSftp || isFtp;

      const hasCredentials = isFileBrowser
        ? (connectionData.authMethod === 'anonymous' || connectionData.authMethod === 'password'
          ? (connectionData.authMethod === 'anonymous' || !!connectionData.password)
          : !!connectionData.privateKeyPath)
        : (connectionData.authMethod === 'password'
          ? !!connectionData.password
          : !!connectionData.privateKeyPath);

      if (!hasCredentials) {
        setEditingConnection(toConnectionConfig(connectionData));
        setConnectionDialogOpen(true);
        return;
      }

      if (!await confirmConnection(connectionData)) return;

      // Always use a unique session ID — the backend may still hold a stale
      // session from a previously closed tab that was never disconnected.
      // A fresh session ID guarantees a new TCP connection with the latest config.
      const sessionId = `${connection.id}-dup-${Date.now()}`;

      if (isFileBrowser) {
        // SFTP/FTP connect flow
        const newTab: TerminalTab = {
          id: sessionId,
          name: connectionData.name,
          tabType: 'file-browser',
          protocol: connectionData.protocol,
          host: connectionData.host,
          username: connectionData.username,
          originalConnectionId: connection.id,
          connectionStatus: 'connecting',
          reconnectCount: 0,
        };
        dispatch({ type: 'ADD_TAB', groupId: state.activeGroupId, tab: newTab });

        try {
          if (isSftp) {
            await invoke('sftp_connect', {
              request: {
                connection_id: sessionId,
                host: connectionData.host,
                port: connectionData.port || 22,
                username: connectionData.username,
                auth_method: connectionData.authMethod || 'password',
                password: connectionData.password || '',
                key_path: connectionData.privateKeyPath || null,
                passphrase: connectionData.passphrase || null,
              }
            });
          } else {
            await invoke('ftp_connect', {
              request: {
                connection_id: sessionId,
                host: connectionData.host,
                port: connectionData.port || 21,
                username: connectionData.username || '',
                password: connectionData.password || '',
                ftps_enabled: connectionData.ftpsEnabled ?? false,
                anonymous: connectionData.authMethod === 'anonymous',
              }
            });
          }
          ConnectionStorageManager.updateLastConnected(connection.id);
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId: sessionId, status: 'connected' });
        } catch (error) {
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId: sessionId, status: 'disconnected' });
          toast.error(t('app.connectionFailed'), {
            description: error instanceof Error ? error.message : String(error),
          });
        }
      } else {
        // SSH connect flow — create a placeholder tab first (shows "Waiting for
        // connection..." so the user knows something is happening), then ssh_connect.
        // Only after ssh_connect succeeds do we switch to 'connecting' status, which
        // triggers PtyTerminal to mount and establish the WebSocket + PTY session.
        // This avoids a race where PtyTerminal sends StartPty before the backend
        // SSH session is fully established.
        const newTab: TerminalTab = {
          id: sessionId,
          name: connectionData.name,
          protocol: connectionData.protocol,
          host: connectionData.host,
          username: connectionData.username,
          originalConnectionId: connection.id,
          connectionStatus: 'pending',
          reconnectCount: 0,
        };
        dispatch({ type: 'ADD_TAB', groupId: state.activeGroupId, tab: newTab });

        console.debug('[SSH] Connecting:', { id: connectionData.id, host: connectionData.host, port: connectionData.port, authMethod: connectionData.authMethod });

        try {
          const result = await invoke<{ success: boolean; error?: string }>(
            'ssh_connect',
            {
              request: buildSshConnectRequest(sessionId, connectionData),
            }
          );

          if (result.success) {
            ConnectionStorageManager.updateLastConnected(connection.id);
            // Switch to 'connecting' — this mounts PtyTerminal which opens WebSocket
            // and sends StartPty. The backend SSH session is ready by now.
            dispatch({ type: 'UPDATE_TAB_STATUS', tabId: sessionId, status: 'connecting' });
          } else {
            console.error('SSH connection failed:', result.error);
            dispatch({ type: 'UPDATE_TAB_STATUS', tabId: sessionId, status: 'disconnected' });
            toast.error(t('app.connectionFailed'), {
              description: result.error || 'Unable to connect to the server. Please check your credentials and try again.',
            });
            setEditingConnection(toConnectionConfig(connectionData));
            setConnectionDialogOpen(true);
          }
        } catch (error) {
          console.error('Error connecting to SSH:', error);
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId: sessionId, status: 'disconnected' });
          toast.error(t('app.connectionError'), {
            description: error instanceof Error ? error.message : t('app.connectionErrorDesc'),
          });
          setEditingConnection(toConnectionConfig(connectionData));
          setConnectionDialogOpen(true);
        }
      }
    }
  };

  const handleTabSelect = useCallback((tabId: string) => {
    // Find which group contains this tab and activate it
    for (const group of Object.values(state.groups)) {
      if (group.tabs.some(t => t.id === tabId)) {
        dispatch({ type: 'ACTIVATE_GROUP', groupId: group.id });
        dispatch({ type: 'ACTIVATE_TAB', groupId: group.id, tabId });
        break;
      }
    }
  }, [state.groups, dispatch]);

  const handleCloseTab = useCallback(async (tabId: string) => {
    // Find which group contains this tab and remove it
    for (const group of Object.values(state.groups)) {
      const tab = group.tabs.find(t => t.id === tabId);
      if (tab) {
        // Disconnect SFTP/FTP sessions when closing file-browser tabs
        if (tab.tabType === 'file-browser') {
          try {
            if (tab.protocol === 'SFTP') {
              await invoke('sftp_standalone_disconnect', { connection_id: tabId });
            } else if (tab.protocol === 'FTP') {
              await invoke('ftp_disconnect', { connection_id: tabId });
            }
          } catch {
            // Ignore disconnect errors on tab close
          }
        }
        dispatch({ type: 'REMOVE_TAB', groupId: group.id, tabId });
        break;
      }
    }
  }, [state.groups, dispatch]);

  // Close every tab in a group. Runs backend cleanup for file-browser
  // sessions first (CLOSE_ALL_TABS is reducer-only and would otherwise
  // leave SFTP/FTP connections alive), then empties the group.
  const handleCloseAllTabs = useCallback(async (groupId: string) => {
    const group = state.groups[groupId];
    if (!group) return;
    for (const tab of group.tabs) {
      // Disconnect SFTP/FTP sessions when closing file-browser tabs
      if (tab.tabType === 'file-browser') {
        try {
          if (tab.protocol === 'SFTP') {
            await invoke('sftp_standalone_disconnect', { connection_id: tab.id });
          } else if (tab.protocol === 'FTP') {
            await invoke('ftp_disconnect', { connection_id: tab.id });
          }
        } catch {
          // Ignore disconnect errors on tab close
        }
      }
    }
    dispatch({ type: 'CLOSE_ALL_TABS', groupId });
  }, [state.groups, dispatch]);

  const handleNewTab = useCallback((folderPath?: string) => {
    setConnectionInitialFolder(folderPath);
    setConnectionDialogOpen(true);
    setEditingConnection(null);
  }, []);

  const handleDuplicateTab = useCallback(async (tabId: string) => {
    const tabToDuplicate = allTabs.find(tab => tab.id === tabId);
    if (!tabToDuplicate) return;

    const originalConnectionId = tabToDuplicate.originalConnectionId || tabId;
    const connectionData = ConnectionStorageManager.getConnection(originalConnectionId);
    if (!connectionData) {
      toast.error(t('app.cannotDuplicate'), {
        description: t('app.cannotDuplicateDesc'),
      });
      return;
    }

    const isSftp = tabToDuplicate.protocol === 'SFTP' || connectionData.protocol === 'SFTP';
    const isFtp = tabToDuplicate.protocol === 'FTP' || connectionData.protocol === 'FTP';
    const isFileBrowser = isSftp || isFtp;

    const hasCredentials = isFileBrowser
      ? (connectionData.authMethod === 'anonymous' || !!connectionData.password || !!connectionData.privateKeyPath)
      : (connectionData.authMethod === 'password'
        ? !!connectionData.password
        : !!connectionData.privateKeyPath);

    if (!hasCredentials) {
      toast.error(t('app.cannotDuplicate'), {
        description: t('app.noCredentialsDesc'),
      });
      return;
    }

    if (!await confirmConnection(connectionData)) return;

    try {
      const duplicateId = `${originalConnectionId}-dup-${Date.now()}`;

      if (isFileBrowser) {
        // SFTP/FTP duplicate flow
        const duplicatedTab: TerminalTab = {
          id: duplicateId,
          name: tabToDuplicate.name,
          tabType: 'file-browser',
          protocol: tabToDuplicate.protocol,
          host: tabToDuplicate.host,
          username: tabToDuplicate.username,
          originalConnectionId,
          connectionStatus: 'connecting',
          reconnectCount: 0,
        };
        dispatch({ type: 'ADD_TAB', groupId: state.activeGroupId, tab: duplicatedTab });

        try {
          if (isSftp) {
            await invoke('sftp_connect', {
              request: {
                connection_id: duplicateId,
                host: connectionData.host,
                port: connectionData.port || 22,
                username: connectionData.username,
                auth_method: connectionData.authMethod || 'password',
                password: connectionData.password || '',
                key_path: connectionData.privateKeyPath || null,
                passphrase: connectionData.passphrase || null,
              }
            });
          } else {
            await invoke('ftp_connect', {
              request: {
                connection_id: duplicateId,
                host: connectionData.host,
                port: connectionData.port || 21,
                username: connectionData.username || '',
                password: connectionData.password || '',
                ftps_enabled: connectionData.ftpsEnabled ?? false,
                anonymous: connectionData.authMethod === 'anonymous',
              }
            });
          }
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId: duplicateId, status: 'connected' });
          toast.success(t('app.tabDuplicated'), {
            description: t('app.tabDuplicatedDesc', { name: tabToDuplicate.name }),
          });
        } catch (error) {
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId: duplicateId, status: 'disconnected' });
          toast.error(t('app.duplicationFailed'), {
            description: error instanceof Error ? error.message : String(error),
          });
        }
      } else {
        // SSH duplicate flow
        const result = await invoke<{ success: boolean; error?: string }>(
          'ssh_connect',
          {
            request: buildSshConnectRequest(duplicateId, connectionData),
          }
        );

        if (result.success) {
          const duplicatedTab: TerminalTab = {
            id: duplicateId,
            name: tabToDuplicate.name,
            protocol: tabToDuplicate.protocol,
            host: tabToDuplicate.host,
            username: tabToDuplicate.username,
            originalConnectionId,
            connectionStatus: 'connecting',
            reconnectCount: 0,
          };

          dispatch({ type: 'ADD_TAB', groupId: state.activeGroupId, tab: duplicatedTab });

          toast.success(t('app.tabDuplicated'), {
            description: t('app.tabDuplicatedDesc', { name: tabToDuplicate.name }),
          });
        } else {
          toast.error(t('app.duplicationFailed'), {
            description: result.error || 'Unable to establish connection for the duplicated tab.',
          });
        }
      }
    } catch (error) {
      console.error('Error duplicating tab:', error);
      toast.error(t('app.duplicationError'), {
        description: error instanceof Error ? error.message : t('app.duplicationErrorDesc'),
      });
    }
  }, [allTabs, state.activeGroupId, dispatch, confirmConnection, t]);

  const handleReconnect = useCallback(async (tabId: string) => {
    const tabToReconnect = allTabs.find(tab => tab.id === tabId);
    if (!tabToReconnect) return;

    const originalConnectionId = tabToReconnect.originalConnectionId || tabId;
    const connectionData = ConnectionStorageManager.getConnection(originalConnectionId);
    if (!connectionData) {
      toast.error(t('app.cannotReconnect'), {
        description: t('app.cannotReconnectDesc'),
      });
      return;
    }

    const isSftp = tabToReconnect.protocol === 'SFTP' || connectionData.protocol === 'SFTP';
    const isFtp = tabToReconnect.protocol === 'FTP' || connectionData.protocol === 'FTP';
    const isFileBrowser = isSftp || isFtp;

    const hasCredentials = isFileBrowser
      ? (connectionData.authMethod === 'anonymous' || !!connectionData.password || !!connectionData.privateKeyPath)
      : (connectionData.authMethod === 'password'
        ? !!connectionData.password
        : !!connectionData.privateKeyPath);

    if (!hasCredentials) {
      toast.error(t('app.cannotReconnect'), {
        description: t('app.noCredentialsDesc'),
      });
      setEditingConnection(toConnectionConfig(connectionData));
      setConnectionDialogOpen(true);
      return;
    }

    if (!await confirmConnection(connectionData)) return;

    // Update tab status to connecting
    dispatch({ type: 'UPDATE_TAB_STATUS', tabId, status: 'connecting' });

    try {
      if (isFileBrowser) {
        // SFTP/FTP reconnect
        try {
          if (isSftp) {
            await invoke('sftp_standalone_disconnect', { connection_id: tabId });
          } else {
            await invoke('ftp_disconnect', { connection_id: tabId });
          }
        } catch {
          // Ignore errors when disconnecting
        }

        if (isSftp) {
          await invoke('sftp_connect', {
            request: {
              connection_id: tabId,
              host: connectionData.host,
              port: connectionData.port || 22,
              username: connectionData.username,
              auth_method: connectionData.authMethod || 'password',
              password: connectionData.password || '',
              key_path: connectionData.privateKeyPath || null,
              passphrase: connectionData.passphrase || null,
            }
          });
        } else {
          await invoke('ftp_connect', {
            request: {
              connection_id: tabId,
              host: connectionData.host,
              port: connectionData.port || 21,
              username: connectionData.username || '',
              password: connectionData.password || '',
              ftps_enabled: connectionData.ftpsEnabled ?? false,
              anonymous: connectionData.authMethod === 'anonymous',
            }
          });
        }

        if (!tabToReconnect.originalConnectionId) {
          ConnectionStorageManager.updateLastConnected(originalConnectionId);
        }
        dispatch({ type: 'UPDATE_TAB_STATUS', tabId, status: 'connected' });
        toast.success(t('app.reconnected'), {
          description: t('app.reconnectedDesc', { name: tabToReconnect.name }),
        });
      } else {
        // SSH reconnect (existing behavior)
        try {
          await invoke('ssh_disconnect', { connection_id: tabId });
        } catch {
          // Ignore errors when disconnecting
        }

        const result = await invoke<{ success: boolean; error?: string }>(
          'ssh_connect',
          {
            request: buildSshConnectRequest(tabId, connectionData),
          }
        );

        if (result.success) {
          if (!tabToReconnect.originalConnectionId) {
            ConnectionStorageManager.updateLastConnected(originalConnectionId);
          }
          // Remount PtyTerminal so it opens a fresh WebSocket/PTY on the
          // newly re-established SSH connection.
          dispatch({ type: 'RECONNECT_TAB', tabId });
          toast.success(t('app.reconnected'), {
            description: t('app.reconnectedDesc', { name: tabToReconnect.name }),
          });
        } else {
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId, status: 'disconnected' });
          toast.error(t('app.reconnectionFailed'), {
            description: result.error || t('app.reconnectionFailedDesc'),
          });
        }
      }
    } catch (error) {
      console.error('Error reconnecting:', error);
      dispatch({ type: 'UPDATE_TAB_STATUS', tabId, status: 'disconnected' });
      toast.error(t('app.reconnectionError'), {
        description: error instanceof Error ? error.message : t('app.reconnectionErrorDesc'),
      });
    }
  }, [allTabs, dispatch, confirmConnection, t]);

  // Handler: open a remote file in a new Tauri window.
  // The window is centered on whichever monitor the parent window currently
  // occupies, matching the behaviour of VS Code, Chrome, Figma, etc.
  const handleOpenInEditor = useCallback((filePath: string, fileName: string) => {
    if (!activeConnection) return;
    const label = `file-viewer-${Date.now()}`;
    const url = `${window.location.origin}/?mode=file-viewer`
      + `&connectionId=${encodeURIComponent(activeConnection.connectionId)}`
      + `&filePath=${encodeURIComponent(filePath)}`
      + `&fileName=${encodeURIComponent(fileName)}`;

    const WIN_W = 900;
    const WIN_H = 700;

    Promise.all([
      import('@tauri-apps/api/webviewWindow'),
      import('@tauri-apps/api/window'),
    ]).then(async ([{ WebviewWindow }, { getCurrentWindow, currentMonitor }]) => {
      const parentWin = getCurrentWindow();
      const [monitor, scaleFactor] = await Promise.all([
        currentMonitor(),          // standalone function, not a method on Window
        parentWin.scaleFactor(),
      ]);

      // Derive logical (DIP) position centered on the parent's monitor.
      // Falls back to Tauri's built-in centering if monitor info is unavailable.
      let position: { x: number; y: number } | undefined;
      if (monitor) {
        const logicalMonX = monitor.position.x / scaleFactor;
        const logicalMonY = monitor.position.y / scaleFactor;
        const logicalMonW = monitor.size.width / scaleFactor;
        const logicalMonH = monitor.size.height / scaleFactor;
        position = {
          x: Math.round(logicalMonX + (logicalMonW - WIN_W) / 2),
          y: Math.round(logicalMonY + (logicalMonH - WIN_H) / 2),
        };
      }

      const win = new WebviewWindow(label, {
        url,
        title: fileName,
        width: WIN_W,
        height: WIN_H,
        // Use explicit position when available; fall back to primary-monitor center
        ...(position ? position : { center: true }),
        resizable: true,
        decorations: true,
      });
      win.once('tauri://error', (e) => {
        toast.error(t('app.failedToOpenWindow'), { description: String(e.payload) });
      });
    }).catch((err: unknown) => {
      toast.error(t('app.couldNotOpenWindow'), { description: String(err) });
    });
  }, [activeConnection, t]);

  const handleConnectionDialogConnect = useCallback(async (config: ConnectionConfig) => {
    const tabId = config.id || `connection-${Date.now()}`;
    const isSftp = config.protocol === 'SFTP';
    const isFtp = config.protocol === 'FTP';
    const isFileBrowser = isSftp || isFtp;
    const isDesktop = isDesktopProtocol(config.protocol);

    // Check if a tab with this ID already exists in any group
    const existingTab = allTabs.find(tab => tab.id === tabId);

    if (existingTab) {
      // Tab exists - activate it and update status
      for (const group of Object.values(state.groups)) {
        if (group.tabs.some(t => t.id === tabId)) {
          dispatch({ type: 'ACTIVATE_GROUP', groupId: group.id });
          dispatch({ type: 'ACTIVATE_TAB', groupId: group.id, tabId });
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId, status: 'connecting' });
          break;
        }
      }

      // For SFTP/FTP reconnect flow
      if (isFileBrowser) {
        try {
          if (isSftp) {
            await invoke('sftp_connect', {
              request: {
                connection_id: tabId,
                host: config.host,
                port: config.port || 22,
                username: config.username,
                auth_method: config.authMethod || 'password',
                password: config.password || '',
                key_path: config.privateKeyPath || null,
                passphrase: config.passphrase || null,
              }
            });
          } else {
            await invoke('ftp_connect', {
              request: {
                connection_id: tabId,
                host: config.host,
                port: config.port || 21,
                username: config.username || '',
                password: config.password || '',
                ftps_enabled: config.ftpsEnabled ?? false,
                anonymous: config.authMethod === 'anonymous',
              }
            });
          }
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId, status: 'connected' });
        } catch (error) {
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId, status: 'disconnected' });
          toast.error(t('app.connectionFailed'), {
            description: error instanceof Error ? error.message : String(error),
          });
        }
      } else if (isDesktop) {
        // RDP/VNC reconnect flow
        try {
          await invoke('desktop_connect', {
            request: {
              connection_id: tabId,
              host: config.host,
              port: config.port || (config.protocol === 'RDP' ? 3389 : 5900),
              protocol: config.protocol.toLowerCase(),
              username: config.username || '',
              password: config.password || '',
              domain: config.domain || null,
              resolution: config.rdpResolution || '1920x1080',
              color_depth: config.vncColorDepth || 24,
            }
          });
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId, status: 'connected' });
        } catch (error) {
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId, status: 'disconnected' });
          toast.error(t('app.connectionFailed'), {
            description: error instanceof Error ? error.message : String(error),
          });
        }
      } else {
        // SSH / Telnet / Raw / Serial reconnect. The backend SSH session was
        // already (re-)established by the dialog's own ssh_connect invoke, so
        // remount PtyTerminal to open a fresh WebSocket/PTY session on it.
        // (RECONNECT_TAB increments reconnectCount, changing PtyTerminal's key
        // in terminal-tab-portals so it remounts; the tab status self-heals to
        // 'connected' once the new PTY session reports ready.)
        dispatch({ type: 'RECONNECT_TAB', tabId });
      }
    } else {
      if (isDesktop) {
        // For RDP/VNC: create desktop tab and connect
        const newTab: TerminalTab = {
          id: tabId,
          name: config.name,
          tabType: 'desktop',
          protocol: config.protocol,
          host: config.host,
          username: config.username,
          connectionStatus: 'connecting',
          reconnectCount: 0,
        };
        dispatch({ type: 'ADD_TAB', groupId: state.activeGroupId, tab: newTab });

        try {
          await invoke('desktop_connect', {
            request: {
              connection_id: tabId,
              host: config.host,
              port: config.port || (config.protocol === 'RDP' ? 3389 : 5900),
              protocol: config.protocol.toLowerCase(),
              username: config.username || '',
              password: config.password || '',
              domain: config.domain || null,
              resolution: config.rdpResolution || '1920x1080',
              color_depth: config.vncColorDepth || 24,
            }
          });
          ConnectionStorageManager.updateLastConnected(config.id || tabId);
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId, status: 'connected' });
        } catch (error) {
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId, status: 'disconnected' });
          toast.error(t('app.connectionFailed'), {
            description: error instanceof Error ? error.message : String(error),
          });
        }
      } else if (isFileBrowser) {
        // For SFTP/FTP: connect first, then add file-browser tab
        const newTab: TerminalTab = {
          id: tabId,
          name: config.name,
          tabType: 'file-browser',
          protocol: config.protocol,
          host: config.host,
          username: config.username,
          connectionStatus: 'connecting',
          reconnectCount: 0,
        };
        dispatch({ type: 'ADD_TAB', groupId: state.activeGroupId, tab: newTab });

        try {
          if (isSftp) {
            await invoke('sftp_connect', {
              request: {
                connection_id: tabId,
                host: config.host,
                port: config.port || 22,
                username: config.username,
                auth_method: config.authMethod || 'password',
                password: config.password || '',
                key_path: config.privateKeyPath || null,
                passphrase: config.passphrase || null,
              }
            });
          } else {
            await invoke('ftp_connect', {
              request: {
                connection_id: tabId,
                host: config.host,
                port: config.port || 21,
                username: config.username || '',
                password: config.password || '',
                ftps_enabled: config.ftpsEnabled ?? false,
                anonymous: config.authMethod === 'anonymous',
              }
            });
          }
          ConnectionStorageManager.updateLastConnected(config.id || tabId);
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId, status: 'connected' });
        } catch (error) {
          dispatch({ type: 'UPDATE_TAB_STATUS', tabId, status: 'disconnected' });
          toast.error(t('app.connectionFailed'), {
            description: error instanceof Error ? error.message : String(error),
          });
        }
      } else {
        // SSH/Telnet: create terminal tab (existing behavior)
        const newTab: TerminalTab = {
          id: tabId,
          name: config.name,
          protocol: config.protocol,
          host: config.host,
          username: config.username,
          connectionStatus: 'connecting',
          reconnectCount: 0,
        };
        dispatch({ type: 'ADD_TAB', groupId: state.activeGroupId, tab: newTab });
      }
    }
  }, [allTabs, state.groups, state.activeGroupId, dispatch, t]);

  const handleOpenSettings = useCallback(() => {
    setSettingsModalOpen(true);
  }, []);

  // Listen for native macOS menu events forwarded by Rust via app.emit("menu-action", id)
  useEffect(() => {
    const unlistenPromise = listen<string>('menu-action', (event) => {
      switch (event.payload) {
        case 'new_connection':
        case 'new_tab':
          handleNewTab();
          break;
        case 'close_connection':
          handleCloseActiveTab();
          break;
        case 'clone_tab':
          if (activeTab) { handleDuplicateTab(activeTab.id); }
          break;
        case 'find':
          runActiveTerminalCommand('find');
          break;
        case 'clear_screen':
          runActiveTerminalCommand('clear-screen');
          break;
        case 'next_tab':
          if (activeGroup && activeGroup.tabs.length > 1 && activeGroup.activeTabId) {
            const idx = activeGroup.tabs.findIndex(t => t.id === activeGroup.activeTabId);
            if (idx < activeGroup.tabs.length - 1) {
              dispatch({ type: 'ACTIVATE_TAB', groupId: activeGroup.id, tabId: activeGroup.tabs[idx + 1].id });
            }
          }
          break;
        case 'prev_tab':
          if (activeGroup && activeGroup.tabs.length > 1 && activeGroup.activeTabId) {
            const idx = activeGroup.tabs.findIndex(t => t.id === activeGroup.activeTabId);
            if (idx > 0) {
              dispatch({ type: 'ACTIVATE_TAB', groupId: activeGroup.id, tabId: activeGroup.tabs[idx - 1].id });
            }
          }
          break;
        case 'settings':
          handleOpenSettings();
          break;
        case 'check_updates':
          setUpdateCheckSignal((current) => current + 1);
          break;
      }
    });
    return () => { unlistenPromise.then(fn => fn()); };
  }, [activeGroup, activeTab, handleNewTab, handleOpenSettings, handleDuplicateTab, handleCloseActiveTab, runActiveTerminalCommand, dispatch]);

  const handleEditConnection = useCallback((connection: ConnectionNode) => {
    if (connection.type === 'connection') {
      const connectionData = ConnectionStorageManager.getConnection(connection.id);
      if (connectionData) {
        setEditingConnection(toConnectionConfig(connectionData));
        setConnectionDialogOpen(true);
      } else {
        toast.error(t('app.connectionNotFound'), {
          description: t('app.connectionNotFoundDesc1'),
        });
      }
    }
  }, [t]);

  const handleSaveConnection = useCallback(async (config: ConnectionConfig) => {
    if (!config.id) return;

    // Update any open tab name for this connection
    for (const group of Object.values(state.groups)) {
      for (const tab of group.tabs) {
        if (tab.id === config.id || tab.originalConnectionId === config.id) {
          dispatch({ type: 'UPDATE_TAB_NAME', tabId: tab.id, name: config.name });
        }
      }
    }

  }, [state.groups, dispatch]);

  // Get recent connections for quick connect
  const recentConnections = useMemo(() => {
    return ConnectionStorageManager.getRecentConnections(8).map(connection => ({
      id: connection.id,
      name: connection.name,
      host: connection.host,
      username: connection.username,
      port: connection.port,
      lastConnected: connection.lastConnected,
    }));
  }, [allTabs]); // Refresh when tabs change (new connection made)

  // Quick connect handler
  const handleQuickConnect = useCallback(async (connectionId: string) => {
    const existingTab = allTabs.find(tab => tab.id === connectionId || tab.originalConnectionId === connectionId);
    if (existingTab) {
      handleTabSelect(existingTab.id);
      toast.info(t('app.alreadyConnected'), {
        description: t('app.alreadyConnectedDesc', { name: existingTab.name }),
      });
      return;
    }

    const connectionData = ConnectionStorageManager.getConnection(connectionId);
    if (!connectionData) {
      toast.error(t('app.connectionNotFound'), {
        description: t('app.connectionNotFoundDesc2'),
      });
      return;
    }

    const isSftp = connectionData.protocol === 'SFTP';
    const isFtp = connectionData.protocol === 'FTP';
    const isFileBrowser = isSftp || isFtp;

    const hasCredentials = isFileBrowser
      ? (connectionData.authMethod === 'anonymous' || !!connectionData.password || !!connectionData.privateKeyPath)
      : (connectionData.authMethod === 'password'
        ? !!connectionData.password
        : !!connectionData.privateKeyPath);

    if (!hasCredentials) {
      setEditingConnection(toConnectionConfig(connectionData));
      setConnectionDialogOpen(true);
      return;
    }

    if (!await confirmConnection(connectionData)) return;

    if (isFileBrowser) {
      // Route through handleConnectionDialogConnect which handles SFTP/FTP
      const config: ConnectionConfig = toConnectionConfig(connectionData);
      await handleConnectionDialogConnect(config);
      toast.success(t('app.quickConnected'), {
        description: t('app.quickConnectedDesc', { name: connectionData.name }),
      });
    } else {
      // SSH quick connect (existing behavior)
      try {
        const result = await invoke<{ success: boolean; error?: string }>(
          'ssh_connect',
          {
            request: buildSshConnectRequest(connectionData.id, connectionData),
          }
        );

        if (result.success) {
          ConnectionStorageManager.updateLastConnected(connectionData.id);

          const config: ConnectionConfig = toConnectionConfig(connectionData);

          handleConnectionDialogConnect(config);

          toast.success(t('app.quickConnected'), {
            description: t('app.quickConnectedDesc', { name: connectionData.name }),
          });
        } else {
          console.error('Quick connect failed:', result.error);
          toast.error(t('app.connectionFailed'), {
            description: result.error || 'Unable to connect. Please try again.',
          });
          setEditingConnection(toConnectionConfig(connectionData));
          setConnectionDialogOpen(true);
        }
      } catch (error) {
        console.error('Quick connect error:', error);
        toast.error(t('app.connectionError'), {
          description: error instanceof Error ? error.message : t('app.connectionErrorDesc'),
        });
      }
    }
  }, [allTabs, handleTabSelect, handleConnectionDialogConnect, confirmConnection, t]);

  // Derive active connection info for StatusBar (compatible format)
  const statusBarConnection = activeConnection ? {
    name: activeConnection.name,
    protocol: activeConnection.protocol || 'SSH',
    host: activeConnection.host,
    status: activeConnection.status,
  } : undefined;

  // Check if there are any tabs across all groups
  const hasAnyTabs = allTabs.length > 0;
  // Check if the grid has only one empty group (show welcome screen)
  const showWelcomeInMainArea = !hasAnyTabs && Object.keys(state.groups).length <= 1;
  // File-browser tabs already include file management.
  const isFileBrowserTab = activeTab?.tabType === 'file-browser';
  // Desktop tabs (RDP/VNC) also don't need right sidebar or bottom panel
  const isDesktopTab = activeTab?.tabType === 'desktop';
  // Editor tabs are standalone — hide extra panels like file-browser/desktop tabs
  const isEditorTab = activeTab?.tabType === 'editor';
  const hideExtraPanels = isFileBrowserTab || isDesktopTab || isEditorTab;

  return (
    <div className="h-screen flex flex-col bg-background">
      <UpdateChecker checkSignal={updateCheckSignal} />
      {/* Web menu bar – on macOS shows only layout controls (native system menu handles File/Edit); on Windows/Linux shows full menus */}
      <MenuBar
        onNewConnection={handleNewTab}
        onNewTab={handleNewTab}
        onCloseConnection={handleCloseActiveTab}
        onNextTab={() => {
          if (activeGroup && activeGroup.tabs.length > 1 && activeGroup.activeTabId) {
            const currentIndex = activeGroup.tabs.findIndex(t => t.id === activeGroup.activeTabId);
            if (currentIndex < activeGroup.tabs.length - 1) {
              dispatch({ type: 'ACTIVATE_TAB', groupId: activeGroup.id, tabId: activeGroup.tabs[currentIndex + 1].id });
            }
          }
        }}
        onPreviousTab={() => {
          if (activeGroup && activeGroup.tabs.length > 1 && activeGroup.activeTabId) {
            const currentIndex = activeGroup.tabs.findIndex(t => t.id === activeGroup.activeTabId);
            if (currentIndex > 0) {
              dispatch({ type: 'ACTIVATE_TAB', groupId: activeGroup.id, tabId: activeGroup.tabs[currentIndex - 1].id });
            }
          }
        }}
        onCloneTab={() => {
          if (activeTab) {
            void handleDuplicateTab(activeTab.id);
          }
        }}
        onCopy={() => runActiveTerminalCommand('copy')}
        onPaste={() => runActiveTerminalCommand('paste')}
        onSelectAll={() => runActiveTerminalCommand('select-all')}
        onFind={() => runActiveTerminalCommand('find')}
        onFindNext={() => runActiveTerminalCommand('find-next')}
        onFindPrevious={() => runActiveTerminalCommand('find-previous')}
        onClearScreen={() => runActiveTerminalCommand('clear-screen')}
        onOpenSettings={handleOpenSettings}
        onCheckForUpdates={() => setUpdateCheckSignal((current) => current + 1)}
        closeConnectionShortcutLabel={keyboardShortcutSettings.closeTab}
        nextTabShortcutLabel={keyboardShortcutSettings.nextTab}
        previousTabShortcutLabel={keyboardShortcutSettings.prevTab}
        hasActiveConnection={!!activeTab}
        hasActiveTerminal={activeTerminalId !== null}
        canPaste={activeTab?.connectionStatus === 'connected'}
        onToggleLeftSidebar={toggleLeftSidebar}
        onToggleBottomPanel={toggleBottomPanel}
        onToggleZenMode={toggleZenMode}
        onApplyPreset={applyPreset}
        leftSidebarVisible={layout.leftSidebarVisible}
        bottomPanelVisible={layout.bottomPanelVisible && !hideExtraPanels}
        zenMode={layout.zenMode}
      />

      <div className="flex-1 flex overflow-hidden">
        <ResizablePanelGroup direction="horizontal" autoSaveId="r-shell-main-layout">
          {/* Left Sidebar - Connection Manager */}
          {layout.leftSidebarVisible && (
            <>
              <ResizablePanel
                id="left-sidebar"
                order={1}
                defaultSize={layout.leftSidebarSize}
                minSize={12}
                maxSize={30}
                onResize={(size) => setLeftSidebarSize(size)}
              >
                <ConnectionManager
                  onConnectionSelect={handleConnectionSelect}
                  onConnectionConnect={handleConnectionConnect}
                  selectedConnectionId={selectedConnection?.id || null}
                  activeConnections={activeConnectionIds}
                  refreshTrigger={connectionSaveTrigger}
                  onNewConnection={handleNewTab}
                  onEditConnection={handleEditConnection}
                  recentConnections={recentConnections}
                  onQuickConnect={handleQuickConnect}
                />
              </ResizablePanel>

              <ResizableHandle />
            </>
          )}

          {/* Main Content - Grid Renderer replaces ConnectionTabs + single terminal */}
          <ResizablePanel
            id="main-content"
            order={2}
            defaultSize={100 - (layout.leftSidebarVisible ? layout.leftSidebarSize : 0)}
            minSize={30}
          >
            <div className="h-full flex flex-col">
              {showWelcomeInMainArea ? (
                <WelcomeScreen
                  onNewConnection={handleNewTab}
                  onOpenSettings={handleOpenSettings}
                />
              ) : (
                <ResizablePanelGroup direction="vertical" className="flex-1">
                  {/* Terminal Grid Panel */}
                  <ResizablePanel id="terminal-grid" order={1} defaultSize={layout.bottomPanelVisible ? 70 : 100} minSize={30}>
                    <TerminalCallbacksProvider value={{
                      onDuplicateTab: handleDuplicateTab,
                      onNewTab: handleNewTab,
                      onReconnectTab: handleReconnect,
                      closeTabShortcut: keyboardShortcutSettings.closeTab,
                      onWorkingDirectoryChange: handleWorkingDirectoryChange,
                      onCloseTab: handleCloseTab,
                      onCloseAllTabs: handleCloseAllTabs,
                    }}>
                      <ErrorBoundary label={t('app.terminal')}>
                        <GridRenderer node={state.gridLayout} path={[]} />
                      </ErrorBoundary>
                    </TerminalCallbacksProvider>
                  </ResizablePanel>

                  {layout.bottomPanelVisible && !hideExtraPanels && activeConnection && (
                    <>
                      <ResizableHandle />

                      {/* File Browser Panel - uses activeConnection from context */}
                      <ResizablePanel
                        id="file-browser"
                        order={2}
                        defaultSize={layout.bottomPanelSize}
                        minSize={20}
                        maxSize={50}
                        onResize={(size) => setBottomPanelSize(size)}
                      >
                        <ErrorBoundary label={t('app.fileBrowser')}>
                          <IntegratedFileBrowser
                          connectionId={activeConnection.connectionId}
                          host={activeConnection.host}
                          isConnected={activeConnection.status === 'connected'}
                          terminalWorkingDirectory={terminalWorkingDirectories[activeConnection.connectionId]}
                          onClose={() => {}}

                          onOpenInEditor={handleOpenInEditor}
                        />
                        </ErrorBoundary>
                      </ResizablePanel>
                    </>
                  )}
                </ResizablePanelGroup>
              )}
            </div>
          </ResizablePanel>

        </ResizablePanelGroup>
      </div>

      <StatusBar activeConnection={statusBarConnection} />

      {/* Modals */}
      <ConnectionDialog
        open={connectionDialogOpen}
        onOpenChange={(open) => {
          setConnectionDialogOpen(open);
          if (!open) {
            setConnectionInitialFolder(undefined);
            setEditingConnection(null);
            setConnectionSaveTrigger(t => t + 1);
          }
        }}
        onConnect={handleConnectionDialogConnect}
        confirmConnection={confirmConnection}
        onSave={handleSaveConnection}
        editingConnection={editingConnection}
        initialFolder={connectionInitialFolder}
      />

      {confirmationDialog}

      <SettingsModal
        open={settingsModalOpen}
        onOpenChange={setSettingsModalOpen}
        onAppearanceChange={() => {
          // Appearance changes are handled by individual PtyTerminal instances
          // via their own settings listeners in TerminalGroupView
        }}
      />

      <Toaster richColors position="top-right" />
    </div>
  );
}

function ShellApp() {
  return (
    <ErrorBoundary label="R-Shell">
      <LayoutProvider>
        <TerminalGroupProvider>
          <AppContent />
        </TerminalGroupProvider>
      </LayoutProvider>
    </ErrorBoundary>
  );
}

export default function App() {
  const [shellUnlocked, setShellUnlocked] = useState(
    () => !isTauriRuntime() && import.meta.env.MODE === 'test',
  );
  const [phraseConfigured, setPhraseConfigured] = useState(false);
  const [statusError, setStatusError] = useState('');

  useEffect(() => {
    /** 浏览器预览没有本机 DPAPI 存储，单元测试直接渲染应用壳。 */
    if (!isTauriRuntime()) {
      return;
    }

    let active = true;
    void getPrivacyStatus()
      .then((status) => {
        if (!active) return;
        setPhraseConfigured(status.configured === true);
      })
      .catch((error) => {
        if (!active) return;
        setStatusError(getPrivacyErrorMessage(error));
      });

    return () => {
      active = false;
    };
  }, []);

  if (shellUnlocked) {
    return <ShellApp />;
  }

  return (
    <ErrorBoundary label="R-Shell">
      <PrivacyCryptoPage
        configured={phraseConfigured}
        statusError={statusError}
        onUnlock={async () => {
          await initializeEncryptedStorage();
          ConnectionStorageManager.initialize();
          await flushEncryptedStorage();
          setShellUnlocked(true);
        }}
      />
    </ErrorBoundary>
  );
}
