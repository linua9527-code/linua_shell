import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";

export interface FileTransferProgressEvent {
  transfer_id: string;
  connection_id: string;
  bytes_transferred: number;
  total_bytes: number;
}

type ProgressHandler = (
  transferId: string,
  progress: number,
  bytesTransferred: number,
  totalBytes: number,
  speed: number,
) => void;

/** Subscribe to backend upload progress and calculate the display speed. */
export function useFileTransferProgress(
  connectionId: string,
  onProgress: ProgressHandler,
): void {
  const onProgressRef = useRef(onProgress);
  onProgressRef.current = onProgress;

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const samples = new Map<string, { bytes: number; timestamp: number }>();

    void listen<FileTransferProgressEvent>("file-transfer-progress", (event) => {
      if (event.payload.connection_id !== connectionId) return;

      const { transfer_id: transferId, bytes_transferred: bytesTransferred, total_bytes: totalBytes } = event.payload;
      const now = performance.now();
      const previous = samples.get(transferId);
      const elapsedSeconds = previous ? (now - previous.timestamp) / 1000 : 0;
      const speed = previous && elapsedSeconds > 0
        ? Math.max(0, (bytesTransferred - previous.bytes) / elapsedSeconds)
        : 0;
      samples.set(transferId, { bytes: bytesTransferred, timestamp: now });

      const progress = totalBytes > 0
        ? Math.min(100, Math.floor((bytesTransferred / totalBytes) * 100))
        : 0;
      onProgressRef.current(transferId, progress, bytesTransferred, totalBytes, speed);
    }).then((stopListening) => {
      if (disposed) {
        stopListening();
      } else {
        unlisten = stopListening;
      }
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [connectionId]);
}
