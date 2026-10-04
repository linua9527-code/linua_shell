import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { useProxyConnectionConfirmation } from '../components/proxy-connection-confirmation';
import { usesBuiltInProxy, type ConnectionProxySettings } from '../lib/connection-proxy';

const directConnection: ConnectionProxySettings = { protocol: 'SSH', proxyType: 'none' };

function ConfirmationHarness({
  connection,
  onResult,
}: {
  connection: ConnectionProxySettings;
  onResult: (confirmed: boolean) => void;
}) {
  const { confirmConnection, confirmationDialog } = useProxyConnectionConfirmation();

  return (
    <>
      <button onClick={() => { void confirmConnection(connection).then(onResult); }}>Start connection</button>
      {confirmationDialog}
    </>
  );
}

afterEach(cleanup);

describe('built-in proxy detection', () => {
  it.each(['http', 'socks4', 'socks5'])('accepts a configured %s SSH proxy', (proxyType) => {
    expect(usesBuiltInProxy({ protocol: 'SSH', proxyType, proxyHost: 'proxy.example.com' })).toBe(true);
  });

  it.each(['SFTP', 'FTP', 'RDP', 'VNC'])('does not claim %s uses the SSH proxy', (protocol) => {
    expect(usesBuiltInProxy({ protocol, proxyType: 'socks5', proxyHost: 'proxy.example.com' })).toBe(false);
  });

  it.each([
    directConnection,
    { protocol: 'SSH', proxyType: 'socks5', proxyHost: '   ' },
    { protocol: 'SSH', proxyType: 'none', proxyHost: 'proxy.example.com' },
  ])('treats an absent or incomplete proxy as a direct connection', (connection) => {
    expect(usesBuiltInProxy(connection)).toBe(false);
  });
});

describe('proxy connection confirmation', () => {
  it('cancels before proceeding with a direct connection', async () => {
    const onResult = vi.fn();
    render(<ConfirmationHarness connection={directConnection} onResult={onResult} />);

    fireEvent.click(screen.getByRole('button', { name: 'Start connection' }));
    expect(screen.getByRole('alertdialog')).toBeTruthy();
    expect(onResult).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(onResult).toHaveBeenCalledWith(false));
  });

  it('proceeds only after explicit confirmation', async () => {
    const onResult = vi.fn();
    render(<ConfirmationHarness connection={directConnection} onResult={onResult} />);

    fireEvent.click(screen.getByRole('button', { name: 'Start connection' }));
    expect(onResult).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Continue connecting' }));

    await waitFor(() => expect(onResult).toHaveBeenCalledWith(true));
  });

  it('proceeds without a prompt when the SSH proxy is configured', async () => {
    const onResult = vi.fn();
    render(<ConfirmationHarness
      connection={{ protocol: 'SSH', proxyType: 'socks5', proxyHost: 'proxy.example.com' }}
      onResult={onResult}
    />);

    fireEvent.click(screen.getByRole('button', { name: 'Start connection' }));
    await waitFor(() => expect(onResult).toHaveBeenCalledWith(true));
    expect(screen.queryByRole('alertdialog')).toBeNull();
  });

  it('warns for SFTP even when an unused proxy value is stored', () => {
    render(<ConfirmationHarness
      connection={{ protocol: 'SFTP', proxyType: 'socks5', proxyHost: 'proxy.example.com' }}
      onResult={vi.fn()}
    />);

    fireEvent.click(screen.getByRole('button', { name: 'Start connection' }));
    expect(screen.getByRole('alertdialog')).toBeTruthy();
  });
});
