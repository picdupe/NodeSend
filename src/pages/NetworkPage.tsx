import { addressPriority } from '../utils/address';
import type { NetworkStatus } from '../types/network';
import { SendPanel } from '../components/transfer/SendPanel';
import { ReceivePanel } from '../components/transfer/ReceivePanel';
import { TaskList } from '../components/transfer/TaskList';
import { Icon } from '../components/Icon';
import { SidebarToggle } from '../components/AppLayout';
import { PublicPathPrompt, publicPathRejected, publicPathAddress } from '../components/PublicPathPrompt';
import { addEndpoint, errorText } from '../api/network';
import { useRef, useState, type FormEvent } from 'react';
import { useDialogEnter } from '../components/useDialogEnter';
import { useDeviceFavorites } from '../hooks/useDeviceFavorites';

export type TransferPage = 'send' | 'receive' | 'transfers';
const copy: Record<TransferPage, [string, string]> = { send: ['传递，就在身边', '把文件送到另一台设备，简单、直接。'], receive: ['接收，尽在掌握', '每一次传递，都由你确认。'], transfers: ['每一次传递', '查看进度、继续任务，或回顾已经送达的文件。'] };
export function NetworkPage({ mode, status, error, refresh, onNavigate }: { mode: TransferPage; status: NetworkStatus | null; error: string; refresh: () => Promise<void>; onNavigate: (page: TransferPage) => void }) {
  const [manualOpen, setManualOpen] = useState(false);
  const [infoOpen, setInfoOpen] = useState(false);
  const [favoritesOpen, setFavoritesOpen] = useState(false);
  const { favorites, favoriteError, toggleFavorite, removeFavorite } = useDeviceFavorites();
  const [refreshing, setRefreshing] = useState(false);
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
  return <div className={'page transfer-page ' + (mode === 'send' ? 'send-page' : '')}><header className="page-heading"><div><span className="eyebrow">NODESEND / {mode === 'send' ? 'SEND' : mode === 'receive' ? 'RECEIVE' : 'ACTIVITY'}</span><h1>{copy[mode][0]}</h1><p>{copy[mode][1]}</p></div><div className="page-heading-actions">{mode === 'send' && <><button type="button" className="icon-button device-info-button" aria-label="查看本机信息" onClick={() => setInfoOpen(true)}><Icon name="info" /></button><button type="button" className="icon-button device-info-button favorites-button" aria-label="查看收藏设备" disabled={!status} onClick={() => setFavoritesOpen(true)}><Icon name="heart" /></button><button type="button" className="icon-button manual-discovery-button" aria-label="手动发现设备" onClick={() => setManualOpen(true)}><Icon name="discover" /><span>手动发现设备</span></button></>}<button type="button" className={'icon-button refresh-button' + (refreshing ? ' refreshing' : '')} aria-label="刷新设备与任务" aria-busy={refreshing} onClick={runRefresh}><Icon name="refresh" /></button><SidebarToggle /></div></header>
    {error && <div className="notice error" role="alert">{status ? error : '尚未连接到 NodeSend 本机服务。请通过桌面应用打开。'}<details><summary>查看原因</summary>{error}</details></div>}
    {status?.discovery.warnings.map(warning => <div className="notice" key={warning}>{warning}；可尝试手动连接设备。</div>)}
    {!status && !error && <div className="surface empty-state generous">正在连接本机服务…</div>}
    {status && <>{mode === 'send' && <div className="send-shell"><SendPanel status={status} favoritesOpen={favoritesOpen} onCloseFavorites={() => setFavoritesOpen(false)} onRemoveFavorite={removeFavorite} favorites={favorites} favoriteError={favoriteError} onToggleFavorite={toggleFavorite} refresh={refresh} onSent={() => onNavigate('transfers')} /></div>}{mode === 'receive' && <ReceivePanel status={status} refresh={refresh} />}{mode === 'transfers' && <TaskList tasks={status.tasks} refresh={refresh} />}</>}
    {manualOpen && <ManualDiscoveryDialog onClose={() => setManualOpen(false)} onAdded={async () => { setManualOpen(false); await refresh(); }} />}
    {infoOpen && status && <DeviceInfoDialog status={status} onClose={() => setInfoOpen(false)} />}

  </div>;
}

function DeviceInfoDialog({ status, onClose }: { status: NetworkStatus; onClose: () => void }) {
  const addresses = [...status.addresses].sort((a, b) => addressPriority(a) - addressPriority(b));
  return <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget) onClose(); }}><section className="modal surface" role="dialog" aria-modal="true" aria-labelledby="device-info-title"><div className="modal-heading"><div><span className="eyebrow">LOCAL NODE</span><h2 id="device-info-title">设备信息</h2></div><button type="button" className="icon-button" onClick={onClose} title="关闭"><Icon name="close" /></button></div><dl className="detail-list"><dt>设备名称</dt><dd>{status.display_name}</dd><dt>IP 地址</dt><dd className="device-info-addresses">{addresses.length ? addresses.map((address, index) => <span className="device-info-address" key={`${address}-${index}`}>{address}</span>) : '暂无地址'}</dd><dt>端口</dt><dd>TCP {status.tcp_port} · QUIC {status.quic_port}</dd></dl><div className="modal-actions"><button type="button" className="primary" onClick={onClose}>关闭</button></div></section></div>;
}

function ManualDiscoveryDialog({ onClose, onAdded }: { onClose: () => void; onAdded: () => Promise<void> }) {
  const [publicAddress, setPublicAddress] = useState('');
  const [address, setAddress] = useState(''); const [expected, setExpected] = useState(''); const [error, setError] = useState(''); const [busy, setBusy] = useState(false);
  const [publicPrompt, setPublicPrompt] = useState(false); const [publicError, setPublicError] = useState('');
  const formRef = useRef<HTMLFormElement>(null);
  useDialogEnter(!publicPrompt, () => { if (!busy) formRef.current?.requestSubmit(); });
  async function connect(allowPublic: boolean) { await addEndpoint(address.trim(), expected.trim() || null, allowPublic); await onAdded(); }
  async function submit(event: FormEvent) {
    event.preventDefault(); if (busy || publicPrompt || !address.trim()) return; setBusy(true); setError('');
    try { await connect(false); }
    catch (err) { if (publicPathRejected(err)) { setPublicAddress(publicPathAddress(err)); setPublicError(''); setPublicPrompt(true); } else setError(errorText(err)); }
    finally { setBusy(false); }
  }
  async function allowPublicPath() {
    setBusy(true); setPublicError('');
    try { await connect(true); }
    catch (err) { setPublicPrompt(false); setError(errorText(err)); }
    finally { setBusy(false); }
  }
  return <><div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget && !busy) onClose(); }}><form ref={formRef} className="modal surface" onSubmit={submit}><div className="modal-heading"><div><span className="eyebrow">DISCOVER NODE</span><h2>手动发现设备</h2></div><button type="button" className="icon-button" onClick={onClose} title="关闭"><Icon name="close" /></button></div><label>地址<input autoFocus value={address} onChange={event => setAddress(event.target.value)} placeholder="192.168.1.20 或设备主机名（可选 :端口）" required /></label><label>预期 Node ID（可选）<input value={expected} onChange={event => setExpected(event.target.value)} placeholder="用于核对接收方身份" /></label><p className="hint">私有 IP 可直接连接；连接公网地址前会请求确认。</p>{error && <div className="notice error" role="alert">{error}</div>}<div className="modal-actions"><button type="button" onClick={onClose}>取消</button><button className="primary" disabled={busy || !address.trim()}>验证并添加设备</button></div></form></div>{publicPrompt && <PublicPathPrompt address={publicAddress} busy={busy} error={publicError} onCancel={() => setPublicPrompt(false)} onConfirm={allowPublicPath} />}</>;
}
