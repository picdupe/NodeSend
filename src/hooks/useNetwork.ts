import { useCallback, useEffect, useRef, useState } from 'react';
import { getNetworkStatus, errorText } from '../api/network';
import type { NetworkStatus } from '../types/network';
export function useNetwork() {
  const [status, setStatus] = useState<NetworkStatus | null>(null);
  const [error, setError] = useState('');
  const mounted = useRef(true);
  const fetching = useRef(false);
  const samples = useRef(new Map<string, { bytes: number; time: number; active: boolean }>());
  const refresh = useCallback(async () => {
    if (fetching.current) return;
    fetching.current = true;
    try {
      const data = await getNetworkStatus();
      const now = performance.now();
      const next = new Map<string, { bytes: number; time: number; active: boolean }>();
      for (const task of data.tasks) {
        const previous = samples.current.get(task.id);
        const active = task.status === 'transferring';
        task.bytes_per_second = active && previous?.active && now > previous.time
          ? Math.max(0, task.completed_bytes - previous.bytes) * 1000 / (now - previous.time) : 0;
        next.set(task.id, { bytes: task.completed_bytes, time: now, active });
      }
      samples.current = next;
      if (mounted.current) { setStatus(data); setError(''); }
    }
    catch (err) { if (mounted.current) setError(errorText(err)); }
    finally { fetching.current = false; }
  }, []);
  useEffect(() => { mounted.current = true; void refresh(); const timer = window.setInterval(refresh, 1000); return () => { mounted.current = false; window.clearInterval(timer); }; }, [refresh]);
  return { status, error, refresh };
}
