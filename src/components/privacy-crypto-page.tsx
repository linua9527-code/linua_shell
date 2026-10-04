import { useId, useRef, useState, type ChangeEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { open as tauriOpen } from '@tauri-apps/plugin-dialog';
import { readTextFile } from '@tauri-apps/plugin-fs';
import {
  Check,
  Clipboard,
  Eraser,
  FileUp,
  KeyRound,
  Loader2,
  LockKeyhole,
  ShieldCheck,
} from 'lucide-react';
import { Button } from './ui/button';
import { Label } from './ui/label';
import { PasswordInput } from './ui/password-input';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from './ui/select';
import { Textarea } from './ui/textarea';
import { Tooltip, TooltipContent, TooltipTrigger } from './ui/tooltip';
import { isTauriRuntime, verifyPrivacyPhrase } from '@/lib/privacy';
import { decryptText, encryptText } from '@/lib/privacy-crypto';

interface PrivacyCryptoPageProps {
  configured: boolean;
  statusError?: string;
  onUnlock: () => void | Promise<void>;
}

type Operation = 'encrypt' | 'decrypt';

function isSupportedTextFile(fileName: string): boolean {
  return /\.(?:txt|md)$/i.test(fileName);
}

function getFileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

function copyText(value: string): Promise<void> {
  if (navigator.clipboard?.writeText) {
    return navigator.clipboard.writeText(value);
  }

  const element = document.createElement('textarea');
  element.value = value;
  element.style.position = 'fixed';
  element.style.opacity = '0';
  document.body.appendChild(element);
  element.focus();
  element.select();
  const copied = document.execCommand('copy');
  element.remove();

  return copied ? Promise.resolve() : Promise.reject(new Error('Clipboard unavailable'));
}

export function PrivacyCryptoPage({
  configured,
  statusError = '',
  onUnlock,
}: PrivacyCryptoPageProps) {
  const { t } = useTranslation();
  const sourceId = useId();
  const resultId = useId();
  const keyId = useId();
  const browserFileInputRef = useRef<HTMLInputElement>(null);
  const [operation, setOperation] = useState<Operation>('encrypt');
  const [algorithm, setAlgorithm] = useState('AES');
  const [sourceText, setSourceText] = useState('');
  const [keyText, setKeyText] = useState('');
  const [resultText, setResultText] = useState('');
  const [busy, setBusy] = useState(false);
  const [fileBusy, setFileBusy] = useState(false);
  const [error, setError] = useState('');
  const [copied, setCopied] = useState(false);

  const handleOperationChange = (nextOperation: Operation) => {
    setOperation(nextOperation);
    setError('');
    setCopied(false);
  };

  const handleProcess = async () => {
    if (!sourceText) {
      setError(t('privacyCrypto.invalidInput'));
      return;
    }

    setBusy(true);
    setError('');
    setCopied(false);

    try {
      if (operation === 'encrypt') {
        const encrypted = await encryptText(sourceText, keyText);
        setResultText(encrypted);

        if (algorithm === 'AES') {
          const phraseMatches = await verifyPrivacyPhrase(sourceText).catch(() => false);
          if (phraseMatches) {
            try {
              await onUnlock();
            } catch (error) {
              setError(t('app.storageUnavailable', { message: error instanceof Error ? error.message : String(error) }));
            }
            return;
          }
        }
      } else {
        setResultText(await decryptText(sourceText, keyText));
      }
    } catch (error) {
      const detail = error instanceof Error
        ? `${error.name}: ${error.message}`
        : String(error);
      setError(
        operation === 'encrypt'
          ? t('privacyCrypto.encryptionFailedWithDetails', { message: detail })
          : t('privacyCrypto.decryptionFailedWithDetails', { message: detail }),
      );
    } finally {
      setBusy(false);
    }
  };

  const handleCopy = async () => {
    if (!resultText) return;
    try {
      await copyText(resultText);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      setError(t('privacyCrypto.copyFailed'));
    }
  };

  const handleClear = () => {
    setSourceText('');
    setKeyText('');
    setResultText('');
    setError('');
    setCopied(false);
  };

  const applyImportedFile = (fileName: string, content: string) => {
    if (!isSupportedTextFile(fileName)) {
      setError(t('privacyCrypto.unsupportedFile'));
      return;
    }

    setSourceText(content);
    setResultText('');
    setError('');
    setCopied(false);
  };

  const handleImportFile = async () => {
    if (!isTauriRuntime()) {
      browserFileInputRef.current?.click();
      return;
    }

    setFileBusy(true);
    setError('');
    try {
      const selected = await tauriOpen({
        filters: [{ name: t('privacyCrypto.textFiles'), extensions: ['txt', 'md'] }],
        multiple: false,
        directory: false,
      });
      const filePath = Array.isArray(selected) ? selected[0] : selected;
      if (!filePath) return;

      const fileName = getFileName(filePath);
      if (!isSupportedTextFile(fileName)) {
        setError(t('privacyCrypto.unsupportedFile'));
        return;
      }

      applyImportedFile(fileName, await readTextFile(filePath));
    } catch {
      setError(t('privacyCrypto.fileImportFailed'));
    } finally {
      setFileBusy(false);
    }
  };

  const handleBrowserFileChange = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.currentTarget.files?.[0];
    event.currentTarget.value = '';
    if (!file) return;

    setFileBusy(true);
    setError('');
    try {
      applyImportedFile(file.name, await file.text());
    } catch {
      setError(t('privacyCrypto.fileImportFailed'));
    } finally {
      setFileBusy(false);
    }
  };

  const sourceLabel = operation === 'encrypt'
    ? t('privacyCrypto.sourcePlaintext')
    : t('privacyCrypto.sourceCiphertext');
  const resultLabel = operation === 'encrypt'
    ? t('privacyCrypto.resultCiphertext')
    : t('privacyCrypto.resultPlaintext');

  return (
    <main className="min-h-screen bg-background text-foreground">
      <div className="mx-auto flex min-h-screen w-full max-w-6xl flex-col px-5 py-6 sm:px-8 lg:px-10">
        <header className="flex items-center justify-between border-b border-border pb-5">
          <div className="flex min-w-0 items-center gap-3">
            <div className="flex size-10 shrink-0 items-center justify-center rounded-md border border-primary/25 bg-primary/10 text-primary">
              <ShieldCheck className="size-5" />
            </div>
            <div className="min-w-0">
              <h1 className="truncate text-lg font-semibold tracking-tight">{t('privacyCrypto.title')}</h1>
              <div className="mt-1 flex items-center gap-2 text-xs text-muted-foreground">
                <span>{t('privacyCrypto.algorithmAes')}</span>
                <span className="text-border">/</span>
                <span className="inline-flex items-center gap-1">
                  {configured ? <LockKeyhole className="size-3" /> : <KeyRound className="size-3" />}
                  {configured ? t('privacyCrypto.configured') : t('privacyCrypto.notConfigured')}
                </span>
              </div>
            </div>
          </div>

        </header>

        <section className="flex flex-1 flex-col py-7">
          <div className="mb-5 flex flex-wrap items-end justify-between gap-4">
            <div>
              <div className="mb-2 flex items-center gap-2 text-sm font-medium text-muted-foreground">
                <span className="size-2 rounded-full bg-primary" />
                {t('privacyCrypto.workspace')}
              </div>
              <h2 className="text-2xl font-semibold tracking-tight">{t('privacyCrypto.heading')}</h2>
            </div>
            <div className="flex items-center gap-2">
              <div className="flex rounded-md border border-border bg-muted/40 p-1">
                <Button
                  type="button"
                  variant={operation === 'encrypt' ? 'default' : 'ghost'}
                  size="sm"
                  onClick={() => handleOperationChange('encrypt')}
                >
                  {t('privacyCrypto.encrypt')}
                </Button>
                <Button
                  type="button"
                  variant={operation === 'decrypt' ? 'default' : 'ghost'}
                  size="sm"
                  onClick={() => handleOperationChange('decrypt')}
                >
                  {t('privacyCrypto.decrypt')}
                </Button>
              </div>
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    type="button"
                    variant="outline"
                    size="icon"
                    aria-label={t('privacyCrypto.clear')}
                    onClick={handleClear}
                  >
                    <Eraser />
                  </Button>
                </TooltipTrigger>
                <TooltipContent>{t('privacyCrypto.clear')}</TooltipContent>
              </Tooltip>
            </div>
          </div>

          <div className="mb-5 grid gap-4 border-y border-border py-4 md:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
            <div className="space-y-2">
              <Label htmlFor="privacy-algorithm">{t('privacyCrypto.algorithm')}</Label>
              <Select value={algorithm} onValueChange={setAlgorithm}>
                <SelectTrigger id="privacy-algorithm">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="AES">{t('privacyCrypto.algorithmAes')}</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-2">
              <Label htmlFor={keyId}>{t('privacyCrypto.key')}</Label>
              <PasswordInput
                id={keyId}
                value={keyText}
                onChange={(event) => setKeyText(event.target.value)}
                placeholder={t('privacyCrypto.keyPlaceholder')}
              />
            </div>
          </div>

          <div className="grid min-h-[420px] flex-1 gap-4 lg:grid-cols-2">
            <div className="flex min-h-[320px] flex-col border border-border bg-card">
              <div className="flex items-center justify-between border-b border-border px-4 py-3">
                <Label htmlFor={sourceId}>{sourceLabel}</Label>
                <div className="flex items-center gap-2">
                  <span className="text-xs text-muted-foreground">{t('privacyCrypto.input')}</span>
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        className="size-7"
                        aria-label={t('privacyCrypto.importFile')}
                        disabled={busy || fileBusy}
                        onClick={() => void handleImportFile()}
                      >
                        <FileUp />
                      </Button>
                    </TooltipTrigger>
                    <TooltipContent>{t('privacyCrypto.importFile')}</TooltipContent>
                  </Tooltip>
                  <input
                    ref={browserFileInputRef}
                    type="file"
                    accept=".txt,.md,text/plain,text/markdown"
                    className="hidden"
                    onChange={(event) => void handleBrowserFileChange(event)}
                  />
                </div>
              </div>
              <Textarea
                id={sourceId}
                value={sourceText}
                onChange={(event) => {
                  setSourceText(event.target.value);
                  setError('');
                }}
                placeholder={operation === 'encrypt'
                  ? t('privacyCrypto.sourcePlaceholderEncrypt')
                  : t('privacyCrypto.sourcePlaceholderDecrypt')}
                className="min-h-0 flex-1 resize-none rounded-none border-0 bg-transparent p-4 shadow-none focus-visible:ring-0"
              />
            </div>

            <div className="flex min-h-[320px] flex-col border border-border bg-card">
              <div className="flex items-center justify-between border-b border-border px-4 py-3">
                <Label htmlFor={resultId}>{resultLabel}</Label>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      className="size-7"
                      aria-label={t('privacyCrypto.copy')}
                      disabled={!resultText}
                      onClick={() => void handleCopy()}
                    >
                      {copied ? <Check className="text-success" /> : <Clipboard />}
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent>{copied ? t('privacyCrypto.copied') : t('privacyCrypto.copy')}</TooltipContent>
                </Tooltip>
              </div>
              <Textarea
                id={resultId}
                value={resultText}
                readOnly
                placeholder={t('privacyCrypto.resultPlaceholder')}
                className="min-h-0 flex-1 resize-none rounded-none border-0 bg-transparent p-4 shadow-none focus-visible:ring-0"
              />
            </div>
          </div>

          <div className="mt-4 flex min-h-10 items-center justify-between gap-4">
            <p className="min-w-0 truncate text-sm text-destructive" role="alert">
              {statusError || error}
            </p>
            <Button type="button" className="shrink-0" disabled={busy} onClick={() => void handleProcess()}>
              {busy ? <Loader2 className="animate-spin" /> : <LockKeyhole />}
              {busy
                ? t('privacyCrypto.processing')
                : operation === 'encrypt' ? t('privacyCrypto.processEncrypt') : t('privacyCrypto.processDecrypt')}
            </Button>
          </div>
        </section>
      </div>

    </main>
  );
}
