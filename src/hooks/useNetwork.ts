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
        // This is the rate of acknowledged completed-byte growth, not raw NIC
        // throughput; storage sync and cache relay can affect the measurement.
        task.bytes_per_second = active && previous?.active && now > previous.time
          ? Math.max(0, task.completed_bytes - previous.bytes) * 1000 / (now - previous.time) : 0;
        next.set(task.id, { bytes: task.completed_bytes, time: now, active });
      }
      samples.current = next;
      if (mounted.current) { setStatus(data); setError(''); }
    }
    catch (err) {
      if (mounted.current) {
        setError(errorText(err));
        // Do not keep presenting an old rate as if it were current while the
        // status endpoint is unavailable. Preserve task details for context.
        setStatus(previous => previous ? {
          ...previous,
          tasks: previous.tasks.map(task => ({ ...task, bytes_per_second: 0 })),
        } : previous);
      }
    }
    finally { fetching.current = false; }
  }, []);
  useEffect(() => { mounted.current = true; void refresh(); const timer = window.setInterval(refresh, 1000); return () => { mounted.current = false; window.clearInterval(timer); }; }, [refresh]);
  return { status, error, refresh };
}
