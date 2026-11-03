import { useEffect, useState } from 'react';
import { errorText, previewCancel, previewStatus, previewTransfer } from '../api/network';
import type { PreviewStatus } from '../types/network';

/**
 */
export function useSendSelection() {
  const [paths, setPaths] = useState<string[]>([]);
  const [manifest, setManifest] = useState<PreviewStatus | null>(null);
  const [scanError, setScanError] = useState('');
  const [peerId, setPeerId] = useState('');
  const [verifyHash, setVerifyHash] = useState(false);

  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof window.setTimeout> | undefined;

    if (!paths.length) {
      setManifest(null);
      setScanError('');
      void previewCancel().catch(() => {});
      return () => { alive = false; };
    }

    setManifest(null);
    setScanError('');

    async function poll() {
      if (!alive) return;
      try {
        const status = await previewStatus();
        if (!alive) return;
        if (status) {
          setScanError('');
          setManifest(status);
          if (!status.scanning) return;
        }
        timer = window.setTimeout(() => { void poll(); }, 400);
      } catch (error) {
        if (alive) {
          setScanError(errorText(error));
          timer = window.setTimeout(() => { void poll(); }, 1000);
        }
      }
    }

    void previewTransfer(paths).then(status => {
      if (!alive) return;
      setManifest(status);
      if (status.scanning) timer = window.setTimeout(() => { void poll(); }, 400);
    }).catch(error => {
      if (alive) setScanError(errorText(error));
    });

    return () => {
      alive = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [paths]);

  return { paths, setPaths, manifest, scanError, peerId, setPeerId, verifyHash, setVerifyHash };
}

export type SendSelectionState = ReturnType<typeof useSendSelection>;
