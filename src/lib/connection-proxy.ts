export interface ConnectionProxySettings {
  protocol: string;
  proxyType?: string;
  proxyHost?: string;
}

/** 判断连接流程是否实际使用 R-Shell 内置代理。 */
export function usesBuiltInProxy(connection: ConnectionProxySettings): boolean {
  return ['SSH', 'Telnet', 'Raw', 'Serial'].includes(connection.protocol)
    && ['http', 'socks4', 'socks5'].includes(connection.proxyType ?? '')
    && !!connection.proxyHost?.trim();
}
