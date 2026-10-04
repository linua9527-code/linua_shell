import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { usesBuiltInProxy, type ConnectionProxySettings } from '@/lib/connection-proxy';
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from './ui/alert-dialog';

export function useProxyConnectionConfirmation() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const pendingRef = useRef<((confirmed: boolean) => void) | null>(null);

  const resolve = useCallback((confirmed: boolean) => {
    const pending = pendingRef.current;
    pendingRef.current = null;
    setOpen(false);
    pending?.(confirmed);
  }, []);

  useEffect(() => () => {
    pendingRef.current?.(false);
    pendingRef.current = null;
  }, []);

  const confirmConnection = useCallback((connection: ConnectionProxySettings): Promise<boolean> => {
    if (pendingRef.current) return Promise.resolve(false);
    if (usesBuiltInProxy(connection)) return Promise.resolve(true);

    return new Promise<boolean>((resolvePending) => {
      pendingRef.current = resolvePending;
      setOpen(true);
    });
  }, []);

  const confirmationDialog = (
    <AlertDialog open={open} onOpenChange={(nextOpen) => { if (!nextOpen) resolve(false); }}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t('app.proxyWarningTitle')}</AlertDialogTitle>
          <AlertDialogDescription>{t('app.proxyWarningDescription')}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel onClick={() => resolve(false)}>{t('common.cancel')}</AlertDialogCancel>
          <AlertDialogAction onClick={() => resolve(true)}>{t('app.continueConnecting')}</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );

  return { confirmConnection, confirmationDialog };
}
