import { addressPriority } from '../utils/address';
import type { NetworkStatus } from '../types/network';
import { SendPanel } from '../components/transfer/SendPanel';
import { SendSession } from '../components/transfer/SendSession';
import { ReceivePanel } from '../components/transfer/ReceivePanel';
import { TaskList } from '../components/transfer/TaskList';
import { Icon } from '../components/Icon';
import { SidebarToggle, WindowControls } from '../components/AppLayout';
import { PublicPathPrompt, publicPathRejected, publicPathAddress } from '../components/PublicPathPrompt';
import { addEndpoint, errorText, sendFiles, transferAction } from '../api/network';
import { useEffect, useRef, useState, type FormEvent } from 'react';
import { useDialogEnter } from '../components/useDialogEnter';
import { useDeviceFavorites } from '../hooks/useDeviceFavorites';
import type { SendSelectionState } from '../hooks/useSendSelection';
import { useTranslation } from 'react-i18next';

export type TransferPage = 'send' | 'receive' | 'transfers';
export function NetworkPage({ mode, status, error, refresh, selection, sendTaskId, onSendTaskChange }: { mode: TransferPage; status: NetworkStatus | null; error: string; refresh: () => Promise<void>; selection: SendSelectionState; sendTaskId: string | null; onSendTaskChange: (id: string | null) => void }) {
  const { t } = useTranslation('common');
  const [manualOpen, setManualOpen] = useState(false);
  const [infoOpen, setInfoOpen] = useState(false);
  const [favoritesOpen, setFavoritesOpen] = useState(false);
  const { favorites, favoriteError, toggleFavorite, removeFavorite } = useDeviceFavorites();
  const [refreshing, setRefreshing] = useState(false);
  const [cancelError, setCancelError] = useState('');
  function cancelSend(id: string) {
    onSendTaskChange(null);
    setCancelError('');
    void transferAction(id, 'cancel').then(refresh).catch(e => setCancelError(errorText(e)));
  }
  async function runRefresh() {
    if (refreshing) return;
    setRefreshing(true);
    const started = Date.now();
    try { await refresh(); } finally {
      const rest = 900 - (Date.now() - started);
      if (rest > 0) await new Promise(resolve => window.setTimeout(resolve, rest));
      setRefreshing(false);
    }
  }
  const sendTask = sendTaskId && status ? status.tasks.find(task => task.id === sendTaskId) : null;
  useEffect(() => {
    if (sendTaskId && sendTask && ['accepted', 'transferring', 'paused', 'remote_paused', 'completed'].includes(sendTask.status)) onSendTaskChange(null);
  }, [mode, sendTaskId, sendTask?.status, onSendTaskChange]);
  if (mode === 'send' && sendTaskId && status && sendTask && ['waiting', 'rejected', 'cancelled', 'failed', 'incompatible'].includes(sendTask.status)) return <SendSession taskId={sendTaskId} status={status} onCancel={cancelSend} onClose={() => onSendTaskChange(null)} />;
  return <div className={'page transfer-page ' + (mode === 'send' ? 'send-page' : mode === 'transfers' ? 'activity-page' : '')}><header className="page-heading"><div><span className="eyebrow">NODESEND / {mode === 'send' ? 'SEND' : mode === 'receive' ? 'RECEIVE' : 'ACTIVITY'}</span></div><div className="page-heading-actions">{mode === 'send' && <><button type="button" className="icon-button device-info-button" aria-label={t('network.deviceInfo')} onClick={() => setInfoOpen(true)}><Icon name="info" /></button><button type="button" className="icon-button device-info-button" aria-label={t('favorites.title')} disabled={!status} onClick={() => setFavoritesOpen(true)}><Icon name="heart" /></button><button type="button" className="icon-button manual-discovery-button" aria-label={t('network.manualDiscovery')} onClick={() => setManualOpen(true)}><Icon name="discover" /><span>{t('network.manualDiscovery')}</span></button></>}<button type="button" className={'icon-button refresh-button' + (refreshing ? ' refreshing' : '')} aria-label={t('network.refresh')} aria-busy={refreshing} onClick={runRefresh}><Icon name="refresh" /></button><SidebarToggle /><WindowControls /></div></header>
    {error && <div className="notice error" role="alert">{status ? error : t('network.localServiceMissing')}<details><summary>{t('network.viewReason')}</summary>{error}</details></div>}
    {cancelError && <div className="notice error" role="alert">{t('network.cancelFailed', { error: cancelError })}</div>}
    {status?.discovery.warnings.map(warning => <div className="notice" key={warning}>{warning.startsWith('mDNS:') ? t('network.mdnsWarning', { reason: warning.slice(5).trim() }) : warning.startsWith('UDP:') ? t('network.udpWarning', { reason: warning.slice(4).trim() }) : t('network.discoveryWarning', { reason: warning })}</div>)}
    {!status && !error && <div className="surface empty-state generous">{t('network.loadingService')}</div>}
    {status && <>{mode === 'send' && <div className="send-shell"><SendPanel status={status} selection={selection} favoritesOpen={favoritesOpen} onCloseFavorites={() => setFavoritesOpen(false)} onRemoveFavorite={removeFavorite} favorites={favorites} favoriteError={favoriteError} onToggleFavorite={toggleFavorite} refresh={refresh} onSent={onSendTaskChange} /></div>}{mode === 'receive' && <ReceivePanel status={status} refresh={refresh} />}{mode === 'transfers' && <TaskList tasks={status.tasks} refresh={refresh} />}</>}
    {manualOpen && <ManualDiscoveryDialog selection={selection} onClose={() => setManualOpen(false)} onAdded={async () => { setManualOpen(false); await refresh(); }} onSent={id => { setManualOpen(false); void refresh(); onSendTaskChange(id); }} />}
    {infoOpen && status && <DeviceInfoDialog status={status} onClose={() => setInfoOpen(false)} />}

  </div>;
}

function DeviceInfoDialog({ status, onClose }: { status: NetworkStatus; onClose: () => void }) {
  const { t } = useTranslation('common');
  const addresses = [...status.addresses].sort((a, b) => addressPriority(a) - addressPriority(b));
  return <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget) onClose(); }}><section className="modal surface" role="dialog" aria-modal="true" aria-labelledby="device-info-title"><div className="modal-heading"><div><span className="eyebrow">LOCAL NODE</span><h2 id="device-info-title">{t('network.deviceInfo')}</h2></div><button type="button" className="icon-button" onClick={onClose} title={t('actions.close')}><Icon name="close" /></button></div><dl className="detail-list"><dt>{t('network.deviceName')}</dt><dd>{status.display_name}</dd><dt>{t('network.ipAddress')}</dt><dd className="device-info-addresses">{addresses.length ? addresses.map((address, index) => <span className="device-info-address" key={`${address}-${index}`}>{address}</span>) : t('network.noAddresses')}</dd><dt>{t('network.port')}</dt><dd>TCP {status.tcp_port} · QUIC {status.quic_port}</dd></dl><div className="modal-actions"><button type="button" className="primary" onClick={onClose}>{t('actions.close')}</button></div></section></div>;
}

function ManualDiscoveryDialog({ selection, onClose, onAdded, onSent }: { selection: SendSelectionState; onClose: () => void; onAdded: () => Promise<void>; onSent: (id: string) => void }) {
  const { t } = useTranslation('common');
  const [publicAddress, setPublicAddress] = useState('');
  const [address, setAddress] = useState(''); const [expected, setExpected] = useState(''); const [error, setError] = useState(''); const [busy, setBusy] = useState(false);
  const [publicPrompt, setPublicPrompt] = useState(false); const [publicSend, setPublicSend] = useState(false); const [publicError, setPublicError] = useState('');
  const formRef = useRef<HTMLFormElement>(null);
  useDialogEnter(!publicPrompt, () => { if (!busy) formRef.current?.requestSubmit(); });
  async function connect(allowPublic: boolean, send = false) {
    const peer = await addEndpoint(address.trim(), expected.trim() || null, allowPublic);
    if (send) {
      if (!selection.paths.length) throw new Error(t('network.chooseFilesFirst'));
      const id = await sendFiles(peer.node_id, selection.paths, selection.verifyHash, allowPublic, address.trim());
      selection.setPaths([]);
      onSent(id);
      return;
    }
    await onAdded();
  }
  async function submit(event: FormEvent) {
    event.preventDefault(); if (busy || publicPrompt || !address.trim()) return; setBusy(true); setError('');
    try { await connect(false); }
    catch (err) { if (publicPathRejected(err)) { setPublicAddress(publicPathAddress(err)); setPublicError(''); setPublicPrompt(true); } else setError(errorText(err)); }
    finally { setBusy(false); }
  }
  async function allowPublicPath() {
    setBusy(true); setPublicError('');
    try { await connect(true, publicSend); }
    catch (err) { setPublicPrompt(false); setError(errorText(err)); }
    finally { setBusy(false); }
  }
  async function sendToAddress() {
    if (!selection.paths.length) { setError(t('network.chooseFilesFirst')); return; }
    if (busy || publicPrompt || !address.trim()) return;
    setBusy(true); setError('');
    try { await connect(false, true); }
    catch (err) { if (publicPathRejected(err)) { setPublicAddress(publicPathAddress(err)); setPublicError(''); setPublicSend(true); setPublicPrompt(true); } else setError(errorText(err)); }
    finally { setBusy(false); }
  }
  return <><div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget && !busy) onClose(); }}><form ref={formRef} className="modal surface" onSubmit={selection.paths.length ? event => { event.preventDefault(); void sendToAddress(); } : submit}><div className="modal-heading"><div><span className="eyebrow">DISCOVER NODE</span><h2>{t('network.manualDiscovery')}</h2></div><button type="button" className="icon-button" onClick={onClose} title={t('actions.close')}><Icon name="close" /></button></div><label>{t('network.discoverAddress')}<input autoFocus value={address} onChange={event => setAddress(event.target.value)} placeholder={t('network.addressPlaceholder')} required /></label><label>{t('network.expectedNodeId')}<input value={expected} onChange={event => setExpected(event.target.value)} placeholder={t('network.expectedNodeHint')} /></label><p className="hint">{t('network.publicHint')}</p>{error && <div className="notice error" role="alert">{error}</div>}<div className="modal-actions"><button type="button" onClick={onClose}>{t('actions.cancel')}</button>{selection.paths.length ? <button type="submit" className="primary" disabled={busy || !address.trim()}>{t('network.verifySend')}</button> : <button type="submit" className="primary" disabled={busy || !address.trim()}>{t('network.verifyAdd')}</button>}</div></form></div>{publicPrompt && <PublicPathPrompt address={publicAddress} busy={busy} error={publicError} onCancel={() => setPublicPrompt(false)} onConfirm={allowPublicPath} />}</>;
}
