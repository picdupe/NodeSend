import { useState } from 'react';
import { decideIncoming, errorText } from '../api/network';
import type { ConflictPolicy } from '../types/network';

export function useReceiveDecision(refresh: () => Promise<void>) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  async function decide(id: string, accepted: boolean, destination: string, conflict: ConflictPolicy = 'rename', selected: string[] | null = null, trustDevice = false) {
    setBusy(true); setError('');
    try { await decideIncoming(id, accepted, destination, conflict, selected, accepted && trustDevice); await refresh(); }
    catch (value) { const message = errorText(value); setError(message); throw value; }
    finally { setBusy(false); }
  }
  return { busy, error, setError, decide };
}
