import { FavoritesDialog } from '../FavoritesDialog';
import { addressPriority } from '../../utils/address';
import { useEffect, useState } from 'react';
import { readImage, readText } from '@tauri-apps/plugin-clipboard-manager';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { addEndpoint, cancelSendPreflight, chooseSendFiles, previewTransfer, removePeer, sendFiles, errorText, stageTextPayload, stageBinaryPayload, readClipboardFiles } from '../../api/network';
import type { Manifest, NetworkStatus, Peer } from '../../types/network';
import type { FavoriteDevice } from '../../hooks/useDeviceFavorites';
import { Icon } from '../Icon';
import { PublicPathPrompt, publicPathRejected, publicPathAddress } from '../PublicPathPrompt';
import { formatBytes } from './format';

export function SendPanel({ status, refresh, onSent, favorites, favoriteError, onToggleFavorite, favoritesOpen, onCloseFavorites, onRemoveFavorite }: { status: NetworkStatus; refresh: () => Promise<void>; onSent: () => void; favorites: FavoriteDevice[]; favoriteError: string; onToggleFavorite: (peer: Peer) => void; favoritesOpen: boolean; onCloseFavorites: () => void; onRemoveFavorite: (nodeId: string) => void }) {
  const [paths, setPaths] = useState<string[]>([]);
  const [manifest, setManifest] = useState<Manifest | null>(null);
  const [peerId, setPeerId] = useState('');
  const [busy, setBusy] = useState(false);
  const [preflighting, setPreflighting] = useState(false);
  const [preflightPeerId, setPreflightPeerId] = useState<string | null>(null);
  const [cancellingPreflight, setCancellingPreflight] = useState(false);
  const [error, setError] = useState('');
  const [dragging, setDragging] = useState(false);
  const [verifyHash, setVerifyHash] = useState(false);
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
    const onSharedFiles = (event: Event) => {
      const files = (event as CustomEvent<string[]>).detail;
      if (Array.isArray(files) && files.length) setPaths(old => [...new Set([...old, ...files])]);
    };
    window.addEventListener('nodesend-shared-files', onSharedFiles);
    return () => window.removeEventListener('nodesend-shared-files', onSharedFiles);
  }, []);
  useEffect(() => {
    let alive = true;
    if (!paths.length) { setManifest(null); return; }
    setManifest(null); setError('');
    previewTransfer(paths).then(m => { if (alive) setManifest(m); }).catch(e => { if (alive) setError(errorText(e)); });
    return () => { alive = false; };
  }, [paths]);
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
      }).catch(e => setError('链接已打开，但设备验证失败：' + errorText(e))).finally(() => setBusy(false));
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
          if (!context) throw new Error('无法读取剪贴板图像。');
          context.putImageData(new ImageData(new Uint8ClampedArray(rgba), size.width, size.height), 0, 0);
          const blob = await new Promise<Blob>((resolve, reject) => canvas.toBlob(value => value ? resolve(value) : reject(new Error('无法保存剪贴板图像。')), 'image/png'));
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
  async function send(allowPublic = false) {
    if (busy || preflighting || !peerId || !manifest) return;
    const selectedPeer = status.peers.find(peer => peer.node_id === peerId);
    if (!selectedPeer) { setError('未找到接收设备，请刷新设备列表后重试。'); return; }
    setPreflighting(true); setPreflightPeerId(peerId); setError('');
    try { await sendFiles(peerId, paths, verifyHash, allowPublic); setPaths([]); await refresh(); onSent(); }
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
  const count = manifest?.entries.filter(e => !e.directory).length ?? 0;
  const total = manifest?.entries.reduce((n, e) => n + e.size, 0) ?? 0;
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
      <div className="send-grid">
      <section className="surface payload-panel">
        <div className="section-title"><span className="step">1</span><h2>选择要发送的内容</h2></div>
        <div className={'drop-zone ' + (dragging ? 'dragging' : '')}>
          <span className="large-icon"><Icon name="folder" size={32} /></span>
          <h3>{dragging ? '松开即可添加' : '选择发送内容'}</h3>
          <p>文件、文件夹、媒体或应用</p>
        <div className={'button-row send-type-buttons ' + (isMobile ? 'mobile-send-types' : 'desktop-send-types')}><button onClick={() => choose(false)}><Icon name="file" size={22} /> <span>文件</span></button><button onClick={() => choose(true)}><Icon name="folder" size={22} /> <span>文件夹</span></button>{isMobile && <><button onClick={() => choose(false, 'media')}><Icon name="image" size={22} /> <span>媒体</span></button><button onClick={() => choose(false, 'apps')}><Icon name="app" size={22} /> <span>应用</span></button></>}<button onClick={() => void addText(false)}><Icon name="text" size={22} /> <span>文本</span></button><button onClick={() => void addText(true)}><Icon name="clipboard" size={22} /> <span>剪切板</span></button></div>
        </div>
        {!!paths.length && <div className="selection-list">
          <div className="selection-heading"><strong>{manifest ? count + ' 个文件 · ' + formatBytes(total) : '正在读取文件清单…'}</strong><button className="text-button" onClick={() => setPaths([])}>清空</button></div>
          {paths.map(path => <div className="selection-row" key={path}><Icon name="file" /><span title={path}>{path.split(/[\\/]/).pop()}</span><button className="icon-button" title="移除" aria-label={'移除 ' + path} onClick={() => setPaths(old => old.filter(p => p !== path))}><Icon name="close" size={16} /></button></div>)}
        </div>}
        <label className="check-label"><input type="checkbox" checked={verifyHash} onChange={e => setVerifyHash(e.target.checked)} /><span className="check-mark" aria-hidden="true"><Icon name="check" size={14} /></span><span>额外校验每个分块的 SHA-256</span></label>
        <p className="caption">传输始终加密。额外哈希校验适合重要资料。</p>
      </section>
      <section className="surface devices-panel">
        <div className="section-title"><span className="step">2</span><h2>发送给谁</h2><span className="count-tag">{status.peers.filter(p => p.online).length} 在线</span></div>
        <input className="search-field" aria-label="搜索设备" placeholder="搜索设备名称、地址或 Node ID" value={search} onChange={e => setSearch(e.target.value)} />
        <div className="device-list">
          {peers.map(peer => <button className={'device-card ' + (peerId === peer.node_id ? 'selected' : '')} key={peer.node_id} onClick={() => { setPeerId(peer.node_id); setContextPeer(null); }} onContextMenu={event => { event.preventDefault(); setContextPeer(peer.node_id); }} onKeyDown={event => { if (event.key === 'Delete') { event.preventDefault(); void forgetPeer(peer.node_id); } }} aria-keyshortcuts="Delete" aria-pressed={peerId === peer.node_id}>
            <span className="device-icon"><Icon name="device" size={25} /></span>
            <span className="device-copy"><strong>{peer.display_name}</strong><span>{peer.endpoints[0]?.address || '等待网络地址'}</span><small>{peer.node_id.slice(0, 12)} · {peer.trusted ? '已信任' : peer.verified ? '身份已验证' : '连接时验证身份'}</small></span>
            {favoriteIds.has(peer.node_id) && <span className="device-favorite-mark" aria-label="已收藏"><Icon name="heart" size={16} /></span>}
            <span className={'status-dot ' + (peer.online ? 'online' : '')} />
            {peerId === peer.node_id && <Icon name="check" size={18} />}
          </button>)}
          {!peers.length && <div className="empty-state"><span className="radar"><Icon name="device" size={28} /></span><h3>正在寻找附近的设备</h3><p>在另一台设备打开 NodeSend。<br />连接同一网络，即可在这里找到彼此。</p><small>也可以手动添加 IP 或主机名</small></div>}
        </div>
      </section>
      </div>
    </div>
    {favoritesOpen && !contextPeer && <FavoritesDialog favorites={favorites} peers={status.peers} error={favoriteError} selectedId={peerId} canSend={!busy && !preflighting && !!manifest} onSelect={setPeerId} onDetails={setContextPeer} onSend={() => { onCloseFavorites(); void send(); }} onRemove={onRemoveFavorite} onClose={onCloseFavorites} />}
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
        <div className="modal-heading"><div><span className="eyebrow">DEVICE DETAILS</span><h2 id="peer-details-title">{peer.display_name}</h2></div><button type="button" className="icon-button" aria-label="关闭设备详情" onClick={() => setContextPeer(null)}><Icon name="close" /></button></div>
        {favoriteError && <div className="notice error" role="alert">{favoriteError}</div>}<dl className="detail-list"><dt>状态</dt><dd>{peer.online ? '在线' : '暂未连接'}</dd><dt>Node ID</dt><dd className="mono">{peer.node_id}</dd><dt>地址</dt><dd className="device-info-addresses">{detailAddresses.length ? detailAddresses.map((address, index) => <span className="device-info-address" key={`${address}-${index}`}>{address}</span>) : '暂无地址'}</dd><dt>端口</dt><dd>{Array.from(new Set(uniqueEndpoints(peer).map(endpoint => `${endpoint.tcp_port} / ${endpoint.quic_port}`))).join('、') || '暂无'}</dd><dt>身份</dt><dd>{peer.trusted ? '已信任' : peer.verified ? '已验证' : '连接时验证'}</dd></dl>
        <div className="modal-actions peer-detail-actions"><button type="button" aria-pressed={favorites.some(item => item.node_id === peer.node_id)} onClick={() => onToggleFavorite(peer)}>{favorites.some(item => item.node_id === peer.node_id) ? '取消收藏' : '加入收藏夹'}</button><button type="button" onClick={() => { setContextPeer(null); void forgetPeer(peer.node_id); }}>移除此设备</button></div>
      </section></div>;
    })()}
    {textDialog === 'text' && <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget) setTextDialog(null); }}><section className="modal surface" role="dialog" aria-modal="true" aria-labelledby="text-dialog-title"><div className="modal-heading"><div><span className="eyebrow">SEND TEXT</span><h2 id="text-dialog-title">输入要发送的文本</h2></div><button type="button" className="icon-button" aria-label="关闭" onClick={() => setTextDialog(null)}><Icon name="close" /></button></div><textarea className="text-entry" autoFocus value={textDraft} onChange={event => setTextDraft(event.target.value)} placeholder="输入文本内容" rows={6} /><div className="modal-actions"><button type="button" onClick={() => setTextDialog(null)}>取消</button><button type="button" className="primary" disabled={!textDraft.trim()} onClick={() => void confirmText()}>添加到发送列表</button></div></section></div>}
    <div className="send-footer"><div><strong>{peerId ? '接收方：' + (status.peers.find(p => p.node_id === peerId)?.display_name ?? '') : '选择内容和接收方后即可发送'}</strong><span>接收方确认后才开始传输 · 无需账号</span></div>{preflighting ? <button className="primary send-button" disabled={cancellingPreflight} onClick={() => void cancelPreflight()}>取消</button> : <button className="primary send-button" disabled={busy || !manifest || !peerId} onClick={() => void send()}>发送{count ? ' ' + count + ' 个文件' : ''}</button>}</div>
    {publicPrompt && <PublicPathPrompt address={publicAddress} busy={busy} error="" onCancel={() => setPublicPrompt(false)} onConfirm={() => void send(true)} />}
  </div>;
}
