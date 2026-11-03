import { FavoritesDialog } from '../FavoritesDialog';
import { addressPriority } from '../../utils/address';
import { useEffect, useState } from 'react';
import { readImage, readText } from '@tauri-apps/plugin-clipboard-manager';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { addEndpoint, cancelSendPreflight, chooseSendFiles, removePeer, sendFiles, errorText, stageTextPayload, stageBinaryPayload, readClipboardFiles } from '../../api/network';
import type { NetworkStatus, Peer } from '../../types/network';
import type { FavoriteDevice } from '../../hooks/useDeviceFavorites';
import type { SendSelectionState } from '../../hooks/useSendSelection';
import { Icon } from '../Icon';
import { PublicPathPrompt, publicPathRejected, publicPathAddress } from '../PublicPathPrompt';
import { formatBytes } from './format';
import { useTranslation } from 'react-i18next';

export function SendPanel({ status, refresh, onSent, favorites, favoriteError, onToggleFavorite, favoritesOpen, onCloseFavorites, onRemoveFavorite, selection }: { status: NetworkStatus; refresh: () => Promise<void>; onSent: (id: string) => void; favorites: FavoriteDevice[]; favoriteError: string; onToggleFavorite: (peer: Peer) => void; favoritesOpen: boolean; onCloseFavorites: () => void; onRemoveFavorite: (nodeId: string) => void; selection: SendSelectionState }) {
  const { t } = useTranslation('transfer');
  const { paths, setPaths, manifest, scanError, peerId, setPeerId, verifyHash, setVerifyHash } = selection;
  const [busy, setBusy] = useState(false);
  const [preflighting, setPreflighting] = useState(false);
  const [preflightPeerId, setPreflightPeerId] = useState<string | null>(null);
  const [cancellingPreflight, setCancellingPreflight] = useState(false);
  const [error, setError] = useState('');
  const [dragging, setDragging] = useState(false);
  const [publicPrompt, setPublicPrompt] = useState(false);
  const [search, setSearch] = useState('');
  const [contextPeer, setContextPeer] = useState<string | null>(null);
  const [textDialog, setTextDialog] = useState<'text' | null>(null);
  const [textDraft, setTextDraft] = useState('');
  const [isMobile, setIsMobile] = useState(() => window.matchMedia?.('(max-width: 850px)').matches ?? false);
  useEffect(() => {
    const query = window.matchMedia?.('(max-width: 850px)');
    if (!query) return;
    const update = () => setIsMobile(query.matches);
    query.addEventListener?.('change', update);
    return () => query.removeEventListener?.('change', update);
  }, []);
  useEffect(() => {
    let disposed = false; let cleanup: (() => void) | undefined;
    try { getCurrentWebview().onDragDropEvent(event => {
      if (event.payload.type === 'over' || event.payload.type === 'enter') setDragging(true);
      else setDragging(false);
      if (event.payload.type === 'drop') { const dropped = event.payload.paths; setPaths(old => [...new Set([...old, ...dropped])]); }
    }).then(unlisten => { if (disposed) unlisten(); else cleanup = unlisten; }).catch(() => {}); } catch { /* Web preview has no native file access. */ }
    return () => { disposed = true; cleanup?.(); };
  }, []);
  useEffect(() => {
    const onDeepLink = (event: Event) => {
      const detail = (event as CustomEvent<{ kind?: string; address?: string; nodeId?: string }>).detail;
      if (detail.kind !== 'node' || !detail.address) return;
      const expectedId = detail.nodeId ?? '';
      setBusy(true);
      void addEndpoint(detail.address, expectedId || null, false).then(peer => {
        setPeerId(peer.node_id); return refresh();
      }).catch(e => setError(t('send.deepLinkVerifyFailed', { error: errorText(e) }))).finally(() => setBusy(false));
    };
    window.addEventListener('nodesend-deep-link', onDeepLink);
    return () => window.removeEventListener('nodesend-deep-link', onDeepLink);
  }, []);
  async function choose(directory: boolean, kind: 'files' | 'media' | 'apps' = 'files') {
    setError('');
    try { const files = await chooseSendFiles(directory, kind); setPaths(old => [...new Set([...old, ...files])]); } catch (e) { setError(errorText(e)); }
  }
  async function addText(fromClipboard: boolean) {
    setError('');
    try {
      if (!fromClipboard) { setTextDraft(''); setTextDialog('text'); return; }
        const files = await readClipboardFiles();
        if (files.length) { setPaths(old => [...new Set([...old, ...files])]); return; }
        try {
          const clipboard = await readText();
          const value = clipboard.split(/\r?\n/)[0]?.trim() ?? '';
          if (value) {
            const staged = await stageTextPayload(value);
            setPaths(old => [...new Set([...old, staged])]);
            return;
          }
        } catch {
          const image = await readImage();
          const [rgba, size] = await Promise.all([image.rgba(), image.size()]);
          const canvas = document.createElement('canvas');
          canvas.width = size.width;
          canvas.height = size.height;
          const context = canvas.getContext('2d');
          if (!context) throw new Error(t('send.clipboardImageReadFailed'));
          context.putImageData(new ImageData(new Uint8ClampedArray(rgba), size.width, size.height), 0, 0);
          const blob = await new Promise<Blob>((resolve, reject) => canvas.toBlob(value => value ? resolve(value) : reject(new Error(t('send.clipboardImageSaveFailed'))), 'image/png'));
          const bytes = Array.from(new Uint8Array(await blob.arrayBuffer()));
          const staged = await stageBinaryPayload(bytes, 'png');
          setPaths(old => [...new Set([...old, staged])]);
        }
    } catch (e) { setError(errorText(e)); }
  }
  async function confirmText() {
    const value = textDraft.trim();
    if (!value) return;
    try {
      const staged = await stageTextPayload(value);
      setPaths(old => [...new Set([...old, staged])]);
      setTextDialog(null);
    } catch (e) { setError(errorText(e)); }
  }
  const [publicAddress, setPublicAddress] = useState('');
  async function send(allowPublic = false, targetPeerId = peerId) {
    if (busy || preflighting || !targetPeerId || !paths.length) return;
    const selectedPeer = status.peers.find(peer => peer.node_id === targetPeerId);
    if (!selectedPeer) { setError(t('send.noPeerFound')); return; }
    setPreflighting(true); setPreflightPeerId(targetPeerId); setError('');
    try { const id = await sendFiles(targetPeerId, paths, verifyHash, allowPublic); setPaths([]); await refresh(); onSent(id); }
    catch (e) { if (!allowPublic && publicPathRejected(e)) { setPublicAddress(publicPathAddress(e)); setPublicPrompt(true); } else { setPublicPrompt(false); setError(errorText(e)); } }
    finally { setPreflighting(false); setPreflightPeerId(null); setCancellingPreflight(false); }
  }
  async function cancelPreflight() {
    if (!preflightPeerId || cancellingPreflight) return;
    setCancellingPreflight(true);
    try { await cancelSendPreflight(preflightPeerId); }
    catch (e) { setError(errorText(e)); setCancellingPreflight(false); }
  }
  async function forgetPeer(nodeId: string) {
    setError('');
    try { await removePeer(nodeId); if (peerId === nodeId) setPeerId(''); if (contextPeer === nodeId) setContextPeer(null); await refresh(); }
    catch (e) { setError(errorText(e)); }
  }
  const favoriteIds = new Set(favorites.map(item => item.node_id));
  const peers = status.peers
    .filter(p => (p.display_name + p.node_id + p.endpoints.map(e => e.address)).toLowerCase().includes(search.toLowerCase()))
    .sort((a, b) => Number(b.online) - Number(a.online)
      || Number(favoriteIds.has(b.node_id)) - Number(favoriteIds.has(a.node_id)));
  const count = manifest?.file_count ?? 0;
  const total = manifest?.total_size ?? 0;
  const uniqueEndpoints = (peer: typeof status.peers[number]) => {
    const seen = new Set<string>();
    return peer.endpoints.filter(endpoint => {
      const address = endpoint.address.replace(/%[^\]:]+(?=\]|$)/, '');
      const key = `${address}|${endpoint.tcp_port}|${endpoint.quic_port}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
  };
  return <div className="send-panel">
    <div className="send-scroll">
      {error && <div className="notice error" role="alert">{error}</div>}
      {scanError && <div className="notice error" role="alert">{t('send.scanError', { error: errorText(scanError) })}</div>}
      <div className="send-grid">
      <section className="surface payload-panel">
        <div className={'drop-zone ' + (dragging ? 'dragging' : '')}>
          <span className="large-icon"><Icon name="folder" size={32} /></span>
          <h3>{dragging ? t('send.releaseToAdd') : t('send.title')}</h3>
          <p>{t('send.subtitle')}</p>
        <div className={'button-row send-type-buttons ' + (isMobile ? 'mobile-send-types' : 'desktop-send-types')}><button onClick={() => choose(false)}><Icon name="file" size={22} /> <span>{t('send.file')}</span></button><button onClick={() => choose(true)}><Icon name="folder" size={22} /> <span>{t('send.folder')}</span></button>{isMobile && <><button onClick={() => choose(false, 'media')}><Icon name="image" size={22} /> <span>{t('send.media')}</span></button><button onClick={() => choose(false, 'apps')}><Icon name="app" size={22} /> <span>{t('send.apps')}</span></button></>}<button onClick={() => void addText(false)}><Icon name="text" size={22} /> <span>{t('send.text')}</span></button><button onClick={() => void addText(true)}><Icon name="clipboard" size={22} /> <span>{t('send.clipboard')}</span></button></div>
        </div>
        {!!paths.length && <div className="selection-list">
          <div className="selection-heading"><strong>{!manifest ? t('send.scan') : manifest.scanning ? (manifest.exceeded ? t('send.tooManyFiles') : t('send.scan')) : t('send.fileCount', { count }) + ' · ' + formatBytes(total)}</strong><button className="text-button" onClick={() => setPaths([])}>{t('send.clear')}</button></div>
          {paths.map(path => <div className="selection-row" key={path}><Icon name="file" /><span>{path.split(/[\\/]/).pop()}</span><button className="icon-button" title={t('send.remove')} aria-label={t('send.removePath', { name: path })} onClick={() => setPaths(old => old.filter(p => p !== path))}><Icon name="close" size={16} /></button></div>)}
        </div>}
        <label className="check-label"><input type="checkbox" checked={verifyHash} onChange={e => setVerifyHash(e.target.checked)} /><span className="check-mark" aria-hidden="true"><Icon name="check" size={14} /></span><span>{t('send.verifyHash')}</span></label>
        <p className="caption">{t('send.encryptedHint')}</p>
      </section>
      <section className="surface devices-panel">
        <div className="search-box"><input className="search-field" aria-label={t('send.searchDevices')} placeholder={t('send.searchDevices')} value={search} onChange={e => setSearch(e.target.value)} /><span className="search-online">{t('send.onlineCount', { count: status.peers.filter(p => p.online).length })}</span></div>
        <div className="device-list">
          {peers.map(peer => <button type="button" className={'device-card device-card-main ' + (peerId === peer.node_id ? 'selected' : '')} key={peer.node_id} onClick={() => { setContextPeer(null); if (preflighting && preflightPeerId === peer.node_id) { void cancelPreflight(); } else if (paths.length && !busy && !preflighting) { setPeerId(peer.node_id); void send(false, peer.node_id); } else { setPeerId(peer.node_id); } }} onContextMenu={event => { event.preventDefault(); setContextPeer(peer.node_id); }} onKeyDown={event => { if (event.key === 'Delete') { event.preventDefault(); void forgetPeer(peer.node_id); } }} aria-keyshortcuts="Delete" aria-pressed={peerId === peer.node_id}>
            <span className="device-icon"><Icon name="device" size={25} /></span>
            <span className="device-copy"><strong>{peer.display_name}</strong><span>{peer.endpoints[0]?.address || t('send.waitingAddress')}</span><small>{peer.node_id.slice(0, 12)} · {peer.trusted ? t('common:device.trusted') : peer.verified ? t('common:device.verified') : t('common:device.verifyOnConnect')}</small></span>
            {favoriteIds.has(peer.node_id) && <span className="device-favorite-mark" aria-label={t('send.favorited')}><Icon name="heart" size={16} /></span>}
            <span className={'status-dot ' + (peer.online ? 'online' : '')} />
          </button>)}          {!peers.length && <div className="empty-state"><span className="radar"><Icon name="device" size={28} /></span><h3>{t('send.searchDevices')}</h3><p>{t('send.waitingAddress')}</p><small>{t('send.discover')}</small></div>}
        </div>
      </section>
      </div>
    </div>
    {favoritesOpen && !contextPeer && <FavoritesDialog favorites={favorites} peers={status.peers} error={favoriteError} selectedId={peerId} canSend={!busy && !preflighting && !!paths.length} onSelect={setPeerId} onDetails={setContextPeer} onSend={() => { onCloseFavorites(); void send(); }} onRemove={onRemoveFavorite} onClose={onCloseFavorites} />}
    {contextPeer && (() => {
      const saved = favorites.find(item => item.node_id === contextPeer);
      const livePeer = status.peers.find(item => item.node_id === contextPeer);
      const peer: Peer | undefined = livePeer ?? (saved ? {
        node_id: saved.node_id, display_name: saved.display_name, endpoints: [],
        online: false, trusted: false, verified: false,
      } : undefined);
      if (!peer) return null;
      const detailAddresses = (livePeer ? uniqueEndpoints(peer).map(endpoint => endpoint.address) : saved?.addresses ?? [])
        .map(address => address.replace(/%[^\]:]+(?=\]|$)/, ''))
        .sort((a, b) => addressPriority(a) - addressPriority(b));
      return <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget) setContextPeer(null); }}><section className="modal surface peer-details-modal" role="dialog" aria-modal="true" aria-labelledby="peer-details-title">
        <div className="modal-heading"><div><span className="eyebrow">DEVICE DETAILS</span><h2 id="peer-details-title">{peer.display_name}</h2></div><button type="button" className="icon-button" aria-label={t('send.closeDetails')} onClick={() => setContextPeer(null)}><Icon name="close" /></button></div>
        {favoriteError && <div className="notice error" role="alert">{favoriteError}</div>}<dl className="detail-list"><dt>{t('send.statusLabel')}</dt><dd>{peer.online ? t('common:device.online') : t('common:device.offline')}</dd><dt>{t('common:device.nodeId')}</dt><dd className="mono">{peer.node_id}</dd><dt>{t('send.addressLabel')}</dt><dd className="device-info-addresses">{detailAddresses.length ? detailAddresses.map((address, index) => <span className="device-info-address" key={`${address}-${index}`}>{address}</span>) : t('send.noAddresses')}</dd><dt>{t('common:network.port')}</dt><dd>{Array.from(new Set(uniqueEndpoints(peer).map(endpoint => `${endpoint.tcp_port} / ${endpoint.quic_port}`))).join(', ') || t('send.noPorts')}</dd><dt>{t('send.identityLabel')}</dt><dd>{peer.trusted ? t('common:device.trusted') : peer.verified ? t('send.verified') : t('send.connectVerify')}</dd></dl>
        <div className="modal-actions peer-detail-actions"><button type="button" aria-pressed={favorites.some(item => item.node_id === peer.node_id)} onClick={() => onToggleFavorite(peer)}>{favorites.some(item => item.node_id === peer.node_id) ? t('send.cancelFavorite') : t('send.addFavorite')}</button><button type="button" onClick={() => { setContextPeer(null); void forgetPeer(peer.node_id); }}>{t('send.removeDevice')}</button></div>
      </section></div>;
    })()}
    {textDialog === 'text' && <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget) setTextDialog(null); }}><section className="modal surface" role="dialog" aria-modal="true" aria-labelledby="text-dialog-title"><div className="modal-heading"><div><span className="eyebrow">SEND TEXT</span><h2 id="text-dialog-title">{t('send.textTitle')}</h2></div><button type="button" className="icon-button" aria-label={t('common:actions.close')} onClick={() => setTextDialog(null)}><Icon name="close" /></button></div><textarea className="text-entry" autoFocus value={textDraft} onChange={event => setTextDraft(event.target.value)} placeholder={t('send.textPlaceholder')} rows={6} /><div className="modal-actions"><button type="button" onClick={() => setTextDialog(null)}>{t('common:actions.cancel')}</button><button type="button" className="primary" disabled={!textDraft.trim()} onClick={() => void confirmText()}>{t('send.addText')}</button></div></section></div>}

    {publicPrompt && <PublicPathPrompt address={publicAddress} busy={busy} error="" onCancel={() => setPublicPrompt(false)} onConfirm={() => void send(true)} />}
  </div>;
}
