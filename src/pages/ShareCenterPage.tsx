import { memo, useCallback, useEffect, useLayoutEffect, useRef, useState, type FormEvent } from 'react';
import { createPortal } from 'react-dom';
import { chooseReceiveDirectory, chooseSendFiles, errorText } from '../api/network';
import type { NetworkStatus, Peer } from '../types/network';
import { browseBrowser, chooseBrowserDownloadPath, closeBrowser, connectBrowser, createBrowserDirectory, deleteBrowserConnection, deleteBrowserEntry, downloadBrowserFile, listBrowserConnections, moveBrowserEntry, renameBrowserEntry, reopenBrowser, testBrowserConnection, updateBrowserConnection, uploadBrowserFile } from '../api/browser';
import type { BrowserConnection, BrowserConnectionInput, BrowserEntry, BrowserListing, BrowserProtocol } from '../types/browser';
import { Icon } from '../components/Icon';
import { SidebarToggle, WindowControls } from '../components/AppLayout';
import { SelectMenu } from '../components/SelectMenu';
import { createShare, deleteShare, listShares, rotateShareToken, startShareLink, toggleShare, updateShareConnection } from '../api/ipc';
import type { AccessMode, ShareConfig, ShareProtocol } from '../types';
import { useTranslation } from 'react-i18next';
import { openShareUrl, protocolShareBrowserUrl, shareBrowserUrl } from '../api/httpShare';

function protocolLabel(protocol: BrowserProtocol | ShareProtocol, t: (key: string) => string) { return protocol === 'local' ? t('shares.local') : ({ nodesend: 'NodeSend', http: 'HTTP', webdav: 'WebDAV', smb: 'SMB', ftp: 'FTP' } as const)[protocol]; }
function accessLabel(mode: AccessMode, t: (key: string) => string) { return t(`shares.${mode}`); }
function ModalErrorToast({ message, revision = 0 }: { message: string; revision?: number }) {
  return message ? <div className="save-toast-layer" role="alert" aria-live="assertive"><div key={revision} className="save-toast error modal-save-toast">{message}</div></div> : null;
}
function ModalErrorToastStack({ items }: { items: { id: number; text: string }[] }) {
  return items.length ? <div className="save-toast-layer modal-error-toast-layer" role="alert" aria-live="assertive">{items.map(item => <div key={item.id} className="modal-error-toast-item"><div className="save-toast error modal-save-toast">{item.text}</div></div>)}</div> : null;
}
type ShareCreateDraft = { path: string; access_mode: AccessMode; access_username?: string; access_password?: string; protocol: ShareProtocol; bind_ips: string };
type ShareNetworkInfo = Pick<NetworkStatus, 'addresses' | 'node_id' | 'tcp_port' | 'quic_port'> & {
  peers: Pick<Peer, 'node_id' | 'display_name' | 'online'>[];
};

function ShareCenterPageView({ shares, onSharesChange, network }: { shares: ShareConfig[]; onSharesChange: (shares: ShareConfig[]) => void; network: ShareNetworkInfo | null }) {
  const { t } = useTranslation('common');
  const [connections, setConnections] = useState<BrowserConnection[]>([]);
  const [active, setActive] = useState('');
  const [listing, setListing] = useState<BrowserListing | null>(null);
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const [showConnect, setShowConnect] = useState(false);
  const [incomingShare, setIncomingShare] = useState('');
  const activeRef = useRef('');
  const listingRequestId = useRef(0);
  const listingLocation = useRef({ connectionId: '', path: '' });
  const connectionsRevision = useRef(0);
  const connectionListRequestId = useRef(0);
  const refresh = useCallback(async (): Promise<boolean> => {
    const requestId = ++connectionListRequestId.current;
    const revision = connectionsRevision.current;
    try {
      const items = await listBrowserConnections();
      if (requestId !== connectionListRequestId.current || revision !== connectionsRevision.current) return true;
      setConnections(items);
      const firstConnected = items.find(item => item.connected);
      const activeConnection = items.find(item => item.id === activeRef.current && item.connected);
      if (!activeConnection) {
        listingRequestId.current += 1;
        setListing(null);
        setError('');
        const nextActive = firstConnected?.id ?? '';
        activeRef.current = nextActive;
        setActive(nextActive);
      }
      return true;
    }
    catch (err) {
      if (requestId === connectionListRequestId.current && revision === connectionsRevision.current) setError(errorText(err));
      return false;
    }
  }, []);
  const load = useCallback(async (id: string, path: string) => {
    if (activeRef.current !== id) return;
    listingLocation.current = { connectionId: id, path };
    const requestId = ++listingRequestId.current;
    setError('');
    setListing(null);
    try {
      const nextListing = await browseBrowser(id, path);
      if (requestId !== listingRequestId.current || activeRef.current !== id) return;
      listingLocation.current = { connectionId: id, path: nextListing.path };
      setListing(nextListing);
    } catch (err) {
      if (requestId !== listingRequestId.current || activeRef.current !== id) return;
      setListing(null);
      setError(errorText(err));
    }
  }, []);
  const refreshListing = useCallback((id: string, path: string) => {
    const current = listingLocation.current;
    if (activeRef.current !== id || current.connectionId !== id || current.path !== path) return Promise.resolve();
    return load(id, path);
  }, [load]);
  useEffect(() => {
    let disposed = false;
    let retryTimer: number | undefined;
    const loadConnections = async () => {
      const loaded = await refresh();
      if (!loaded && !disposed) retryTimer = window.setTimeout(() => void loadConnections(), 3000);
    };
    void loadConnections();
    return () => {
      disposed = true;
      if (retryTimer !== undefined) window.clearTimeout(retryTimer);
    };
  }, [refresh]);
  useEffect(() => { if (active) void load(active, ''); }, [active, load]);
  useEffect(() => {
    const onDeepLink = (event: Event) => {
      const detail = (event as CustomEvent<{ kind?: string; httpUrl?: string }>).detail;
      if (detail.kind === 'share' && detail.httpUrl) setIncomingShare(detail.httpUrl);
    };
    window.addEventListener('nodesend-deep-link', onDeepLink);
    return () => window.removeEventListener('nodesend-deep-link', onDeepLink);
  }, []);
  function activate(id: string) {
    if (activeRef.current === id) {
      if (id && !listing && error) void load(id, listingLocation.current.path);
      return;
    }
    listingRequestId.current += 1;
    activeRef.current = id;
    listingLocation.current = { connectionId: id, path: '' };
    setActive(id);
    setListing(null);
    setError('');
  }
  async function disconnect(id: string) {
    try {
      await closeBrowser(id);
    } catch (err) {
      setError(errorText(err));
      await refresh();
      return;
    }
    connectionsRevision.current += 1;
    setConnections(items => items.map(item => item.id === id ? { ...item, connected: false } : item));
    if (activeRef.current === id) {
      const nextActive = connections.find(item => item.id !== id && item.connected)?.id ?? '';
      activate(nextActive);
    }
    await refresh();
  }
  return <div className="page browser-page">
    <header className="page-heading"><div><span className="eyebrow">NODESEND / FILE BROWSER</span></div><div className="page-heading-actions"><SidebarToggle /><WindowControls /></div></header>
    {error && <div className="notice error" role="alert">{error}</div>}{message && <div className="notice success">{message}</div>}
    {incomingShare && <div className="notice success deep-link-notice"><span>{t('shares.openedLink')}</span><a href={incomingShare} target="_blank" rel="noreferrer">{t('shares.openHttp')}</a><button className="text-button" onClick={() => setIncomingShare('')}>{t('actions.close')}</button></div>}
    <div className="browser-tabs" role="tablist">{connections.filter(connection => connection.connected).map(connection => <button key={connection.id} role="tab" aria-selected={active === connection.id} className={active === connection.id ? 'active' : ''} onClick={() => activate(connection.id)}><span>{protocolLabel(connection.protocol, t)}</span><strong>{connection.name}</strong><span className="tab-close" onClick={event => { event.stopPropagation(); void disconnect(connection.id); }}>×</span></button>)}<button className="browser-tab-add" aria-label={t('shares.addConnection')} title={t('shares.addConnection')} onClick={() => setShowConnect(true)}><Icon name="plus" size={16} /></button></div>
    {active && listing?.connection_id === active && <BrowserWorkspace key={active} listing={listing} connection={connections.find(c => c.id === active)!} onNavigate={path => load(active, path)} onRefresh={() => refreshListing(active, listing.path)} setMessage={setMessage} />}
    {!active && <section className="surface empty-state generous"><Icon name="folder" size={40} /><h3>{t('shares.openConnection')}</h3></section>}
    {active && !listing && !error && <section className="surface empty-state generous"><Icon name="folder" size={40} /><h3>{t('loading')}</h3></section>}
    <LocalShares shares={shares} onSharesChange={onSharesChange} network={network} />
    {showConnect && <ConnectDialog saved={connections} network={network} onClose={() => setShowConnect(false)} onConnected={connection => { connectionsRevision.current += 1; setConnections(items => items.filter(item => item.id !== connection.id).concat(connection)); activate(connection.id); setShowConnect(false); void refresh(); }} onChanged={connection => { connectionsRevision.current += 1; setConnections(items => items.map(item => item.id === connection.id ? connection : item)); if (activeRef.current === connection.id) activate(''); void refresh(); }} onDeleted={id => { connectionsRevision.current += 1; setConnections(items => items.filter(item => item.id !== id)); if (activeRef.current === id) activate(''); void refresh(); }} />}
  </div>;
}

export const ShareCenterPage = memo(ShareCenterPageView);

function BrowserWorkspace({ listing, connection, onNavigate, onRefresh, setMessage }: { listing: BrowserListing; connection: BrowserConnection; onNavigate: (path: string) => Promise<void>; onRefresh: () => Promise<void>; setMessage: (message: string) => void }) {
  const { t } = useTranslation('common');
  const [busy, setBusy] = useState(false);
  const [selectedPaths, setSelectedPaths] = useState<string[]>([]);
  const [progress, setProgress] = useState('');
  const selected = listing.entries.filter(entry => selectedPaths.includes(entry.path));
  const detail = selected.length === 1 ? selected[0] : null;
  const canManage = listing.can_write && connection.protocol !== 'nodesend';
  useEffect(() => {
    const currentPaths = new Set(listing.entries.map(entry => entry.path));
    setSelectedPaths(paths => {
      const retained = paths.filter(path => currentPaths.has(path));
      return retained.length === paths.length ? paths : retained;
    });
  }, [listing.connection_id, listing.path, listing.entries]);

  async function download(entry: BrowserEntry) {
    if (busy) return;
    setBusy(true);
    try {
      const destination = await chooseBrowserDownloadPath(entry.name);
      if (!destination) return;
      await downloadBrowserFile(connection.id, entry.path, destination);
      setMessage(t('shares.downloaded'));
    }
    catch (err) { setMessage(errorText(err)); }
    finally { setBusy(false); }
  }
  async function upload() {
    if (busy) return;
    setBusy(true);
    setProgress('');
    let uploaded = 0;
    try {
      const files = await chooseSendFiles(false);
      if (!files.length) return;
      for (let i = 0; i < files.length; i++) {
        const name = files[i].split(/[\\/]/).pop() || 'file';
        await uploadBrowserFile(connection.id, files[i], listing.path ? listing.path + '/' + name : name);
        uploaded += 1;
        setProgress(`${i + 1} / ${files.length}`);
      }
      await onRefresh();
      setMessage(t('shares.uploaded', { count: files.length }));
    } catch (err) {
      setMessage(errorText(err));
      if (uploaded > 0) await onRefresh();
    }
    finally { setBusy(false); }
  }
  async function mkdir() {
    const name = window.prompt(t('shares.newDirectory'));
    if (!name) return;
    setBusy(true);
    try { await createBrowserDirectory(connection.id, listing.path ? listing.path + '/' + name : name); await onRefresh(); }
    catch (err) { setMessage(errorText(err)); }
    finally { setBusy(false); }
  }
  async function refreshListing() {
    if (busy) return;
    setBusy(true);
    try { await onRefresh(); }
    finally { setBusy(false); }
  }
  async function remove(entry: BrowserEntry) {
    if (!window.confirm(t('shares.confirmDelete', { name: entry.name }))) return;
    setBusy(true);
    try { await deleteBrowserEntry(connection.id, entry.path); setSelectedPaths([]); await onRefresh(); }
    catch (err) { setMessage(errorText(err)); }
    finally { setBusy(false); }
  }
  async function removeSelected() {
    if (!selected.length || !window.confirm(t('shares.confirmDeleteSelected', { count: selected.length }))) return;
    setBusy(true);
    try {
      for (const entry of selected) await deleteBrowserEntry(connection.id, entry.path);
      setSelectedPaths([]);
      await onRefresh();
    } catch (err) { setMessage(errorText(err)); await onRefresh(); }
    finally { setBusy(false); }
  }
  async function rename(entry: BrowserEntry) {
    const name = window.prompt(t('shares.newName'), entry.name);
    if (!name || name === entry.name) return;
    setBusy(true);
    try { await renameBrowserEntry(connection.id, entry.path, name); setSelectedPaths([]); await onRefresh(); }
    catch (err) { setMessage(errorText(err)); }
    finally { setBusy(false); }
  }
  async function move(entry: BrowserEntry) {
    const target = window.prompt(t('shares.targetPath'), entry.path);
    if (!target || target === entry.path) return;
    setBusy(true);
    try { await moveBrowserEntry(connection.id, entry.path, target); setSelectedPaths([]); await onRefresh(); }
    catch (err) { setMessage(errorText(err)); }
    finally { setBusy(false); }
  }
  const crumbs = listing.path ? [t('shares.root')].concat(listing.path.split('/')) : [t('shares.root')];
  return <section className="surface browser-workspace">
    <div className="browser-toolbar">
      <div className="breadcrumbs">{crumbs.map((crumb, index) => <button key={crumb + index} onClick={() => { setSelectedPaths([]); onNavigate(index === 0 ? '' : crumbs.slice(1, index + 1).join('/')); }}>{crumb}</button>)}</div>
      <div className="button-row">
        <button disabled={busy || !listing.can_write} onClick={upload}>{t('shares.upload')}</button>
        {connection.protocol !== 'nodesend' && <button disabled={busy || !listing.can_write} onClick={mkdir}>{t('shares.mkdir')}</button>}
        {canManage && selected.length > 0 && <button className="danger" disabled={busy} onClick={removeSelected}>{t('shares.deleteSelected', { count: selected.length })}</button>}
        <button className={'refresh-button' + (busy ? ' refreshing' : '')} disabled={busy} onClick={() => void refreshListing()} aria-label={t('shares.refreshDirectory')} title={t('shares.refreshDirectory')}><Icon name="refresh" size={16} /></button>
      </div>
    </div>
    <div className="browser-location"><span className="badge">{protocolLabel(connection.protocol, t)}</span><span>{connection.address}</span><span className="muted">{connection.protocol === 'nodesend' ? t('shares.nativeTransport') : listing.can_write ? t('shares.readWrite') : t('shares.readOnly')}</span>{busy && progress && <span>{progress}</span>}</div>
    <table className="browser-table"><thead><tr><th><input type="checkbox" aria-label={t('shares.selectAll')} checked={listing.entries.length > 0 && selected.length === listing.entries.length} onChange={event => setSelectedPaths(event.target.checked ? listing.entries.map(entry => entry.path) : [])} /></th><th>{t('shares.name')}</th><th>{t('shares.modified')}</th><th>{t('shares.size')}</th><th>{t('shares.actions')}</th></tr></thead><tbody>
      {listing.entries.map(entry => <tr key={entry.path}>
        <td><input type="checkbox" aria-label={t('shares.selectEntry', { name: entry.name })} checked={selectedPaths.includes(entry.path)} onChange={event => setSelectedPaths(paths => event.target.checked ? [...paths, entry.path] : paths.filter(path => path !== entry.path))} /></td>
        <td><button className="browser-entry-name" onClick={() => entry.is_dir ? (setSelectedPaths([]), onNavigate(entry.path)) : setSelectedPaths([entry.path])}><Icon name={entry.is_dir ? 'folder' : 'file'} size={17} />{entry.name}</button></td>
        <td>{entry.modified || '—'}</td><td>{entry.is_dir ? '—' : formatSize(entry.size)}</td>
        <td className="actions">{!entry.is_dir && <button disabled={busy} onClick={() => void download(entry)}>{t('shares.download')}</button>}{canManage && <><button disabled={busy} onClick={() => void rename(entry)}>{t('shares.rename')}</button><button disabled={busy} onClick={() => void move(entry)}>{t('shares.move')}</button><button className="danger" disabled={busy} onClick={() => void remove(entry)}>{t('shares.delete')}</button></>}</td>
      </tr>)}
      {!listing.entries.length && <tr><td colSpan={5}><div className="empty-state"><Icon name="folder" size={28} /><p>{t('shares.emptyDirectory')}</p></div></td></tr>}
    </tbody></table>
    {detail && <div className="browser-detail"><strong>{detail.name}</strong><span>{detail.is_dir ? t('shares.folder') : t('shares.file')}</span><span>{detail.is_dir ? '—' : formatSize(detail.size)}</span><span>{detail.modified || t('shares.modifiedUnknown')}</span><span className="mono">{detail.path}</span></div>}
  </section>;
}

function ConnectDialog({ saved, network, onClose, onConnected, onChanged, onDeleted }: { saved: BrowserConnection[]; network: ShareNetworkInfo | null; onClose: () => void; onConnected: (connection: BrowserConnection) => void; onChanged: (connection: BrowserConnection) => void; onDeleted: (id: string) => void }) {
  const { t } = useTranslation('common');
  const [protocol, setProtocol] = useState<BrowserProtocol>('nodesend'); const [name, setName] = useState('NodeSend'); const [address, setAddress] = useState(''); const [username, setUsername] = useState(''); const [password, setPassword] = useState(''); const [error, setError] = useState(''); const [busy, setBusy] = useState(false);
  const [details, setDetails] = useState<BrowserConnection | null>(null);
  const peers: Pick<Peer, 'node_id' | 'display_name' | 'online'>[] = network?.peers ?? [];
  async function submit(event: FormEvent) { event.preventDefault(); if (protocol === 'nodesend' && !address) { setError(t('shares.chooseDevice')); return; } setBusy(true); setError(''); const input: BrowserConnectionInput = { name: name, protocol: protocol, address: address, username: username || null, password: password || null }; try { onConnected(await connectBrowser(input)); } catch (err) { setError(errorText(err)); } finally { setBusy(false); } }
  const placeholders: Record<BrowserProtocol, string> = { local: 'D:\\Share', nodesend: '', webdav: 'https://example.com/dav/', smb: '\\\\server\\share', ftp: 'ftp://server:21/path' };
  return <><div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget && !busy) onClose(); }}><form className="modal surface modal-error-host" onSubmit={submit}><div className="modal-heading"><div><span className="eyebrow">NEW CONNECTION</span><h2>{t('shares.newConnection')}</h2></div><button type="button" className="icon-button" disabled={busy} onClick={onClose} title={t('actions.close')}><Icon name="close" /></button></div>{saved.length > 0 && <div className="saved-connections"><h3>{t('shares.savedConnections')}</h3>{saved.map(item => <button type="button" key={item.id} disabled={busy} onClick={() => { if (item.connected) { onConnected(item); return; } setBusy(true); setError(''); void reopenBrowser(item.id).then(onConnected).catch(err => setError(errorText(err))).finally(() => setBusy(false)); }} onContextMenu={event => { event.preventDefault(); if (!busy) setDetails(item); }}><span>{protocolLabel(item.protocol, t)}</span><strong>{item.name}</strong><Icon name="arrow" size={15} /></button>)}</div>}<label>{t('shares.protocol')}<SelectMenu<BrowserProtocol> label={t('shares.protocol')} value={protocol} options={[{ value: 'nodesend', label: 'NodeSend' }, { value: 'webdav', label: 'WebDAV' }, { value: 'smb', label: 'SMB' }, { value: 'ftp', label: 'FTP' }]} onChange={value => { setProtocol(value); setName(protocolLabel(value, t)); setAddress(''); setUsername(''); setPassword(''); setError(''); }} disabled={busy} /></label><label>{t('shares.connectionName')}<input value={name} onChange={event => setName(event.target.value)} required disabled={busy} /></label>{protocol === 'nodesend' ? <label>{t('shares.device')}<SelectMenu label={t('shares.device')} value={address} options={[{ value: '', label: t('shares.selectDevice') }, ...peers.map(peer => ({ value: peer.node_id, label: peer.display_name + (peer.online ? '' : t('shares.offline')) }))]} onChange={setAddress} disabled={busy} /></label> : <label>{t('shares.address')}<input value={address} onChange={event => setAddress(event.target.value)} placeholder={placeholders[protocol]} required disabled={busy} /></label>}{protocol !== 'local' && protocol !== 'smb' && <>{protocol !== 'nodesend' && <label>{t('shares.usernameOptional')}<input value={username} onChange={event => setUsername(event.target.value)} disabled={busy} /></label>}<label>{protocol === 'nodesend' ? t('shares.sharedToken') : t('shares.passwordOptional')}<input type="password" value={password} onChange={event => setPassword(event.target.value)} disabled={busy} /></label></>}<ModalErrorToast message={error} /><div className="modal-actions"><button type="button" disabled={busy} onClick={onClose}>{t('actions.cancel')}</button><button className="primary" disabled={busy}>{t('shares.connectOpen')}</button></div></form></div>{details && <SavedConnectionDialog connection={details} onClose={() => setDetails(null)} onChanged={onChanged} onDeleted={onDeleted} />}</>;
}

function SavedConnectionDialog({ connection, onClose, onChanged, onDeleted }: { connection: BrowserConnection; onClose: () => void; onChanged: (connection: BrowserConnection) => void; onDeleted: (id: string) => void }) {
  const { t } = useTranslation('common');
  const [name, setName] = useState(connection.name); const [protocol, setProtocol] = useState<BrowserProtocol>(connection.protocol); const [address, setAddress] = useState(connection.address); const [username, setUsername] = useState(connection.username ?? ''); const [password, setPassword] = useState(''); const [message, setMessage] = useState(''); const [error, setError] = useState(''); const [busy, setBusy] = useState(false);
  const input = (): BrowserConnectionInput => ({ name, protocol, address, username: username || null, password: password || null });
  async function test() { setBusy(true); setMessage(''); setError(''); try { await testBrowserConnection(input()); setMessage(t('shares.testSuccess')); } catch (err) { setError(errorText(err)); } finally { setBusy(false); } }
  async function save() { setBusy(true); setMessage(''); setError(''); try { onChanged(await updateBrowserConnection(connection.id, input())); setMessage(t('shares.saved')); } catch (err) { setError(errorText(err)); } finally { setBusy(false); } }
  async function remove() { setBusy(true); setError(''); try { await deleteBrowserConnection(connection.id); onDeleted(connection.id); onClose(); } catch (err) { setError(errorText(err)); } finally { setBusy(false); } }
  return <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget && !busy) onClose(); }}><section className="modal surface modal-error-host" role="dialog" aria-modal="true" onKeyDown={event => { if (event.key !== 'Enter') return; const target = event.target as HTMLElement; if (target.tagName === 'INPUT') { const inputs = Array.from(event.currentTarget.querySelectorAll<HTMLInputElement>('input')); const index = inputs.indexOf(target as HTMLInputElement); if ((target as HTMLInputElement).type === 'password') { event.preventDefault(); target.blur(); } else if (index >= 0 && index < inputs.length - 1) { event.preventDefault(); inputs[index + 1].focus(); } } else if (!target.closest('button, [role="button"], [role="combobox"], select')) { event.preventDefault(); if (!busy) void save(); } }}><ModalErrorToast message={error} /><div className="modal-heading"><div><span className="eyebrow">SAVED CONNECTION</span><h2>{t('shares.details')}</h2></div><button type="button" className="icon-button" disabled={busy} onClick={onClose} aria-label={t('actions.close')}><Icon name="close" /></button></div><label>{t('shares.protocol')}<SelectMenu<BrowserProtocol> label={t('shares.protocol')} value={protocol} options={[{ value: 'nodesend', label: 'NodeSend' }, { value: 'webdav', label: 'WebDAV' }, { value: 'smb', label: 'SMB' }, { value: 'ftp', label: 'FTP' }]} onChange={setProtocol} disabled={busy} /></label><label>{t('shares.connectionName')}<input value={name} onChange={event => setName(event.target.value)} disabled={busy} /></label><label>{t('shares.address')}<input value={address} onChange={event => setAddress(event.target.value)} disabled={busy} /></label><label>{t('shares.usernameOptional')}<input value={username} onChange={event => setUsername(event.target.value)} disabled={busy} /></label><label>{t('shares.passwordKeep')}<input type="password" value={password} onChange={event => setPassword(event.target.value)} disabled={busy} /></label>{message && <div className="notice" role="status">{message}</div>}<div className="modal-actions"><button type="button" className="danger" disabled={busy} onClick={() => void remove()}>{t('shares.delete')}</button><button type="button" disabled={busy} onClick={() => void test()}>{t('shares.testConnection')}</button><button type="button" className="primary" disabled={busy} onClick={() => void save()}>{t('shares.saveChanges')}</button></div></section></div>;
}

function LocalShares({ shares, onSharesChange, network }: { shares: ShareConfig[]; onSharesChange: (shares: ShareConfig[]) => void; network: ShareNetworkInfo | null }) {
  const { t } = useTranslation('common');
  const [message, setMessage] = useState('');
  const [modalErrorToasts, setModalErrorToasts] = useState<{ id: number; text: string }[]>([]);
  const [errorToasts, setErrorToasts] = useState<{ id: number; text: string }[]>([]);
  const errorToastId = useRef(0);
  const modalErrorToastId = useRef(0);
  const [shareActionBusy, setShareActionBusy] = useState(false);
  const shareActionLock = useRef(false);
  const modalOpenRef = useRef(false);
  const [creating, setCreating] = useState(false);
  const [accessMode, setAccessMode] = useState<AccessMode>('token');
  const [shareProtocol, setShareProtocol] = useState<ShareProtocol>('nodesend');
  const [sharePath, setSharePath] = useState('');
  const [createCredentialsOpen, setCreateCredentialsOpen] = useState(false);
  const [bindIpDialogOpen, setBindIpDialogOpen] = useState(false);
  const [pendingSharePath, setPendingSharePath] = useState('');
  const [bindIps, setBindIps] = useState('');
  const [credentialUsername, setCredentialUsername] = useState('');
  const [credentialPassword, setCredentialPassword] = useState('');
  const [credentialConfirmation, setCredentialConfirmation] = useState('');
  const [editingShare, setEditingShare] = useState<ShareConfig | null>(null);
  const [editUsername, setEditUsername] = useState('');
  const [editPassword, setEditPassword] = useState('');
  const [editPort, setEditPort] = useState('');
  const [editBindIps, setEditBindIps] = useState('');
  const [connectionBusy, setConnectionBusy] = useState(false);
  const refresh = useCallback(async () => {
    const items = await listShares();
    onSharesChange(items);
  }, [onSharesChange]);
  useEffect(() => {
    modalOpenRef.current = createCredentialsOpen || bindIpDialogOpen || !!editingShare;
    if (!createCredentialsOpen && !bindIpDialogOpen && !editingShare) {
      setModalErrorToasts([]);
    } else setErrorToasts([]);
  }, [createCredentialsOpen, bindIpDialogOpen, editingShare]);
  function showModalError(text: string) {
    const id = ++modalErrorToastId.current;
    setModalErrorToasts(current => [...current, { id, text }]);
    window.setTimeout(() => setModalErrorToasts(current => current.filter(item => item.id !== id)), 2600);
  }
  function showErrorToast(text: string) {
    const id = ++errorToastId.current;
    setErrorToasts(current => [...current, { id, text }]);
    window.setTimeout(() => setErrorToasts(current => current.filter(item => item.id !== id)), 2600);
  }
  async function perform(action: () => Promise<unknown>, toastOnError = false, isCreation = false, onError?: (error: unknown) => boolean) {
    if (shareActionLock.current) return;
    shareActionLock.current = true;
    setShareActionBusy(true);
    if (isCreation) setCreating(true);
    setMessage('');
    let actionSucceeded = false;
    try {
      await action();
      actionSucceeded = true;
    } catch (err) {
      const text = errorText(err);
      const handled = onError?.(err) ?? false;
      const modalOpen = createCredentialsOpen || bindIpDialogOpen || !!editingShare;
      if (modalOpen && !handled) showModalError(text);
      else setMessage(handled || toastOnError ? '' : text);
      if (toastOnError && !handled && !modalOpen) {
        showErrorToast(text);
      }
    }
    finally {
      await refresh().catch(err => {
        const text = errorText(err);
        if (!actionSucceeded && modalOpenRef.current) showModalError(text);
        else setMessage(text);
      });
      if (isCreation) setCreating(false);
      setShareActionBusy(false);
      shareActionLock.current = false;
    }
  }
  async function create(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (shareActionBusy || shareActionLock.current) return;
    const form = new FormData(event.currentTarget);
    const path = String(form.get('path'));
    if (accessMode === 'password') {
      setMessage('');
      setPendingSharePath(path);
      setBindIps('');
      setCredentialUsername('');
      setCredentialPassword('');
      setCredentialConfirmation('');
      setCreateCredentialsOpen(true);
      return;
    }
    setMessage('');
    setPendingSharePath(path);
    setBindIps('');
    setBindIpDialogOpen(true);
  }

  async function submitShare(request: ShareCreateDraft, requestedPort: number) {
    if (!request.bind_ips.split(',').some(ip => ip.trim())) {
      showModalError(t('shares.bindIpRequired'));
      return;
    }
    await perform(async () => {
      await createShare({ ...request, port: requestedPort });
      modalOpenRef.current = false;
      setSharePath('');
      setCreateCredentialsOpen(false);
      setBindIpDialogOpen(false);
    }, true, true);
  }
  async function createPasswordShare(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (shareActionBusy || shareActionLock.current) return;
    if (!credentialUsername.trim()) {
      showModalError(t('shares.usernameRequired'));
      return;
    }
    if (credentialPassword !== credentialConfirmation) {
      showModalError(t('shares.passwordMismatch'));
      return;
    }
    await submitShare({
        path: pendingSharePath,
        access_mode: 'password',
        access_username: credentialUsername.trim(),
        access_password: credentialPassword,
        protocol: shareProtocol,
        bind_ips: bindIps,
      }, shareProtocol === 'smb' ? 445 : shareProtocol === 'ftp' ? 21 : 0);
  }

  async function createUnprotectedShare(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (shareActionBusy || shareActionLock.current) return;
    const requestedPort = shareProtocol === 'smb' ? 445 : shareProtocol === 'ftp' ? 21 : 0;
    await submitShare({ path: pendingSharePath, access_mode: accessMode, protocol: shareProtocol, bind_ips: bindIps }, requestedPort);
  }

  function editShareInfo(share: ShareConfig) {
    setMessage('');
    setEditingShare(share);
    setEditUsername(share.access_username ?? '');
    setEditPassword('');
    setEditPort(String(share.port));
    setEditBindIps(share.bind_ips || '');
  }

  async function saveShareInfo(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!editingShare || shareActionBusy || shareActionLock.current) return;
    if (editingShare.access_mode === 'password' && !editUsername.trim()) {
      showModalError(t('shares.usernameRequired'));
      return;
    }
    if (!editBindIps.split(',').some(ip => ip.trim())) {
      showModalError(t('shares.bindIpRequired'));
      return;
    }
    const portText = editPort.trim();
    const port = Number(portText);
    if (!/^\d+$/.test(portText) || !Number.isSafeInteger(port) || port < 0 || port > 65535) {
      showModalError(t('shares.invalidPort'));
      return;
    }
    setConnectionBusy(true);
    try {
      await perform(async () => {
        await updateShareConnection({
          id: editingShare.id,
          ...(editingShare.access_mode === 'password' ? { username: editUsername.trim(), password: editPassword || undefined } : {}),
          port,
          bind_ips: editBindIps,
        });
        modalOpenRef.current = false;
        setEditingShare(null);
      });
    } finally { setConnectionBusy(false); }
  }
  const accessOptions: { value: AccessMode; label: string }[] = shareProtocol === 'nodesend'
    ? [
        { value: 'token', label: t('shares.token') },
        { value: 'open', label: t('shares.open') },
        { value: 'trusted', label: t('shares.trusted') },
      ]
    : shareProtocol === 'http'
      ? [
          { value: 'token', label: t('shares.token') },
          { value: 'password', label: t('shares.password') },
          { value: 'open', label: t('shares.open') },
        ]
      : [
          { value: 'password', label: t('shares.password') },
          { value: 'open', label: t('shares.open') },
        ];
  return <section className="surface local-shares">
    {errorToasts.length > 0 && !createCredentialsOpen && !bindIpDialogOpen && !editingShare && <div className="save-toast-layer share-error-toast-layer" aria-live="assertive">{errorToasts.map(item => <div key={item.id} className="share-error-toast-motion" role="alert"><div className="save-toast error">{item.text}</div></div>)}</div>}
    <div className="section-title"><h2>{t('shares.localShares')}</h2></div>
    <form className="share-form" onSubmit={create}>
      <div className="share-path-picker"><input name="path" aria-label={t('shares.sharePath')} placeholder={t('shares.sharePath')} value={sharePath} onChange={event => setSharePath(event.target.value)} required disabled={creating || shareActionBusy} /><button type="button" disabled={creating || shareActionBusy} onClick={async () => { try { const path = await chooseReceiveDirectory(); if (path) setSharePath(path); } catch (err) { showErrorToast(errorText(err)); } }}>{t('actions.choose')}</button></div>
      <SelectMenu label={t('shares.accessMode')} value={accessMode} options={accessOptions} onChange={setAccessMode} disabled={creating || shareActionBusy} fitContent />
      <SelectMenu label={t('shares.shareProtocol')} value={shareProtocol} options={[{ value: 'nodesend', label: 'NodeSend' }, { value: 'http', label: 'HTTP' }, { value: 'smb', label: 'SMB' }, { value: 'ftp', label: 'FTP' }]} onChange={protocol => { if (protocol === 'nodesend') { if (accessMode === 'password' || accessMode === 'trusted') setAccessMode('token'); } else if (protocol !== 'http' && accessMode !== 'open' && accessMode !== 'password') setAccessMode('password'); else if (accessMode === 'trusted') setAccessMode('password'); setShareProtocol(protocol); }} disabled={creating || shareActionBusy} fitContent />
      <button className="primary" disabled={creating || shareActionBusy}>{t('shares.addFolder')}</button>
    </form>
    {message && !createCredentialsOpen && !bindIpDialogOpen && !editingShare && <p className="hint" role="status">{message}</p>}
    <div className="share-table-wrap"><table className="table"><thead><tr><th>{t('shares.name')}</th><th>{t('common.path')}</th><th>{t('shares.protocol')}</th><th>{t('shares.accessMode')}</th><th>{t('common.status')}</th><th>{t('shares.actions')}</th></tr></thead>
      <tbody>{shares.map(share => {
        const native = share.protocol === 'nodesend';
        const running = share.enabled;
        const rowBusy = shareActionBusy;
        return <tr key={share.id}><td>{share.name}</td><td className="mono">{share.path}</td><td>{protocolLabel(share.protocol, t)}</td>
          <td>{accessLabel(share.access_mode, t)}</td>
          <td>{share.enabled ? t('shares.enabled') : t('shares.disabled')}</td>
          <td className="actions"><button disabled={rowBusy} onClick={() => void perform(() => toggleShare(share.id, !running), true)}>{running ? t('shares.stop') : t('shares.start')}</button>
            {share.access_mode === 'token' && <button disabled={rowBusy} onClick={() => void perform(() => rotateShareToken(share.id), true)}>{t('shares.rotateToken')}</button>}
            {!native && <button disabled={rowBusy} onClick={() => editShareInfo(share)}>{t('shares.editInfo')}</button>}
            {native ? <ShareLinks share={share} network={network} disabled={rowBusy} /> : <ProtocolConnection share={share} network={network} running={running} disabled={rowBusy} />}
            <button disabled={rowBusy} className="danger" onClick={() => void perform(() => deleteShare(share.id), true)}>{t('shares.delete')}</button>
          </td></tr>;
      })}</tbody></table></div>
    {createCredentialsOpen && <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget && !creating && !shareActionBusy) setCreateCredentialsOpen(false); }}><form className="modal surface modal-error-host" role="dialog" aria-modal="true" onSubmit={createPasswordShare}><ModalErrorToastStack items={modalErrorToasts} /><div className="modal-heading"><div><span className="eyebrow">{t('shares.password')}</span><h2>{t('shares.credentials')}</h2></div><button type="button" className="icon-button" disabled={creating || shareActionBusy} onClick={() => setCreateCredentialsOpen(false)} aria-label={t('actions.close')}><Icon name="close" /></button></div><label>{t('shares.username')}<input value={credentialUsername} onChange={event => setCredentialUsername(event.target.value)} autoComplete="username" required disabled={creating || shareActionBusy} /></label><label>{t('shares.password')}<input type="password" value={credentialPassword} onChange={event => setCredentialPassword(event.target.value)} autoComplete="new-password" required disabled={creating || shareActionBusy} /></label><label>{t('shares.confirmPassword')}<input type="password" value={credentialConfirmation} onChange={event => setCredentialConfirmation(event.target.value)} autoComplete="new-password" required disabled={creating || shareActionBusy} /></label><label>{t('shares.bindIp')}<BindIpField value={bindIps} onChange={setBindIps} options={network?.addresses ?? []} disabled={creating || shareActionBusy} /></label><div className="modal-actions"><button type="button" disabled={creating || shareActionBusy} onClick={() => setCreateCredentialsOpen(false)}>{t('actions.cancel')}</button><button className="primary" disabled={creating || shareActionBusy}>{t('shares.addFolder')}</button></div></form></div>}
    {bindIpDialogOpen && <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget && !creating && !shareActionBusy) setBindIpDialogOpen(false); }}><form className="modal surface modal-error-host" role="dialog" aria-modal="true" onSubmit={createUnprotectedShare}><div className="modal-heading"><div><span className="eyebrow">{protocolLabel(shareProtocol, t)}</span><h2>{t('shares.bindIpTitle')}</h2></div><button type="button" className="icon-button" disabled={creating || shareActionBusy} onClick={() => setBindIpDialogOpen(false)} aria-label={t('actions.close')}><Icon name="close" /></button></div><label>{t('shares.bindIp')}<BindIpField value={bindIps} onChange={setBindIps} options={network?.addresses ?? []} disabled={creating || shareActionBusy} /></label><ModalErrorToastStack items={modalErrorToasts} /><div className="modal-actions"><button type="button" disabled={creating || shareActionBusy} onClick={() => setBindIpDialogOpen(false)}>{t('actions.cancel')}</button><button className="primary" disabled={creating || shareActionBusy}>{t('shares.addFolder')}</button></div></form></div>}
    {editingShare && <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget && !connectionBusy && !shareActionBusy) setEditingShare(null); }}><form className="modal surface modal-error-host" role="dialog" aria-modal="true" onSubmit={saveShareInfo}><div className="modal-heading"><div><span className="eyebrow">{protocolLabel(editingShare.protocol, t)}</span><h2>{t('shares.editInfo')}</h2></div><button type="button" className="icon-button" disabled={connectionBusy || shareActionBusy} onClick={() => setEditingShare(null)} aria-label={t('actions.close')}><Icon name="close" /></button></div>{editingShare.access_mode === 'password' && <><label>{t('shares.username')}<input value={editUsername} onChange={event => setEditUsername(event.target.value)} autoComplete="username" required disabled={connectionBusy || shareActionBusy} /></label><label>{t('shares.password')}<input type="password" value={editPassword} onChange={event => setEditPassword(event.target.value)} placeholder={t('shares.passwordUnchanged')} autoComplete="new-password" disabled={connectionBusy || shareActionBusy} /></label></>}<label>{t('shares.listenPort')}<input type="text" inputMode="numeric" value={editPort} onChange={event => setEditPort(event.target.value)} disabled={connectionBusy || shareActionBusy} /></label><label>{t('shares.bindIp')}<BindIpField value={editBindIps} onChange={setEditBindIps} options={network?.addresses ?? []} disabled={connectionBusy || shareActionBusy} /></label><ModalErrorToastStack items={modalErrorToasts} /><div className="modal-actions"><button type="button" disabled={connectionBusy || shareActionBusy} onClick={() => setEditingShare(null)}>{t('actions.cancel')}</button><button className="primary" disabled={connectionBusy || shareActionBusy}>{t('actions.save')}</button></div></form></div>}
  </section>;
}

function BindIpField({ value, onChange, options, disabled }: { value: string; onChange: (value: string) => void; options: string[]; disabled?: boolean }) {
  const { t } = useTranslation('common');
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState('');
  const [menuPosition, setMenuPosition] = useState<{ left: number; top: number; width: number; maxHeight: number } | null>(null);
  const fieldRef = useRef<HTMLDivElement>(null);
  const choices = ['0.0.0.0', '::', ...options].filter((ip, index, all) => all.indexOf(ip) === index);
  const visibleChoices = choices.filter(ip => !filter || ip.toLowerCase().includes(filter));
  useLayoutEffect(() => {
    if (!open || !fieldRef.current) return;
    const updatePosition = () => {
      if (!fieldRef.current) return;
      const field = fieldRef.current.getBoundingClientRect();
      const dialogElement = fieldRef.current.closest('.modal');
      const dialog = dialogElement?.getBoundingClientRect();
      const firstInput = dialogElement?.querySelector('input')?.getBoundingClientRect();
      const boundaryTop = Math.max(8, firstInput && firstInput.top < field.top ? firstInput.top : dialog?.top ?? 8);
      const boundaryBottom = Math.min(window.innerHeight - 8, dialog?.bottom ?? window.innerHeight - 8);
      const spaceAbove = Math.max(0, field.top - boundaryTop - 4);
      const spaceBelow = Math.max(0, boundaryBottom - field.bottom - 4);
      const desiredHeight = Math.max(44, visibleChoices.length * 36 + 12);
      const placeAbove = spaceBelow < desiredHeight && spaceAbove > spaceBelow;
      const maxHeight = Math.max(40, Math.min(desiredHeight, placeAbove ? spaceAbove : spaceBelow));
      setMenuPosition({
        left: field.left,
        top: placeAbove ? field.top - maxHeight - 4 : field.bottom + 4,
        width: field.width,
        maxHeight,
      });
    };
    updatePosition();
    window.addEventListener('resize', updatePosition);
    window.addEventListener('scroll', updatePosition, true);
    return () => {
      window.removeEventListener('resize', updatePosition);
      window.removeEventListener('scroll', updatePosition, true);
    };
  }, [open, visibleChoices.length]);
  function choose(ip: string) {
    const parts = value.split(',');
    const previous = parts.slice(0, -1).map(part => part.trim()).filter(Boolean);
    previous.push(ip);
    onChange(previous.join(','));
    setFilter('');
    setOpen(false);
  }
  return <>
  <div ref={fieldRef} className="share-ip-field" onBlur={event => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setOpen(false); }}>
    <input aria-haspopup="listbox" aria-expanded={open} value={value} onFocus={() => { setFilter(''); setOpen(true); }} onClick={() => setOpen(true)} onChange={event => { onChange(event.target.value); setFilter(event.target.value.split(',').pop()?.trim().toLowerCase() ?? ''); }} onKeyDown={event => { if (event.key === 'Escape') setOpen(false); }} autoComplete="off" disabled={disabled} />
  </div>
  {open && !disabled && menuPosition && createPortal(<div className="share-ip-options" role="listbox" style={{ position: 'fixed', left: menuPosition.left, top: menuPosition.top, width: menuPosition.width, maxHeight: menuPosition.maxHeight }}>{visibleChoices.map(ip => <button type="button" role="option" key={ip} onMouseDown={event => event.preventDefault()} onClick={() => choose(ip)}>{ip}</button>)}{visibleChoices.length === 0 && <span>{t('shares.noIpMatches')}</span>}</div>, document.body)}
  </>;
}

function shareConnectionHost(share: ShareConfig, network: ShareNetworkInfo | null): string {
  const configured = (share.bind_ips || '').split(',').map(ip => ip.trim()).filter(Boolean);
  const concrete = configured.find(ip => ip !== '0.0.0.0' && ip !== '::');
  if (concrete) return concrete;
  const addresses = network?.addresses ?? [];
  const ipv4Wildcard = configured.includes('0.0.0.0');
  const ipv6Wildcard = configured.includes('::');
  const ipv4 = addresses.find(ip => ip.includes('.') && ip !== '0.0.0.0' && !ip.startsWith('127.'));
  const ipv6 = addresses.find(ip => ip.includes(':') && ip !== '::' && ip !== '::1');
  if (ipv4Wildcard && !ipv6Wildcard) return ipv4 ?? '127.0.0.1';
  if (ipv6Wildcard && !ipv4Wildcard) return ipv6 ?? '::1';
  return ipv4 ?? ipv6 ?? '127.0.0.1';
}

function formatUrlHost(host: string): string {
  if (!host.includes(':')) return host;
  if (host.startsWith('[') && host.endsWith(']')) return host;
  const zoneIndex = host.indexOf('%');
  const address = zoneIndex < 0 ? host : `${host.slice(0, zoneIndex)}%25${host.slice(zoneIndex + 1)}`;
  return `[${address}]`;
}

function ProtocolConnection({ share, network, running, disabled = false }: { share: ShareConfig; network: ShareNetworkInfo | null; running: boolean; disabled?: boolean }) {
  const { t } = useTranslation('common');
  const [open, setOpen] = useState(false);
  const [openError, setOpenError] = useState('');
  const host = shareConnectionHost(share, network);
  const urlHost = formatUrlHost(host);
  const protocolShareName = share.protocol_share_name || share.name;
  const ftpUsername = share.access_mode === 'password' ? share.access_username ?? '' : protocolShareName;
  const address = share.protocol === 'http' ? protocolShareBrowserUrl(`http://${urlHost}:${share.port}`, share) : share.protocol === 'ftp' ? `ftp://${encodeURIComponent(ftpUsername)}@${urlHost}:${share.port}/${encodeURIComponent(protocolShareName)}/` : `smb://${urlHost}:${share.port}/${encodeURIComponent(protocolShareName)}`;
  const webdavAddress = `http://${urlHost}:${share.port}/${encodeURIComponent(protocolShareName)}/dav/`;
  async function copyAddress() {
    setOpenError('');
    try { await navigator.clipboard.writeText(address); }
    catch { setOpenError(t('shares.copyFailed')); }
  }
  return <><button className={!network ? 'share-info-pending' : undefined} disabled={disabled || !network} onClick={() => { if (!network) return; setOpenError(''); setOpen(true); }}>{t('shares.connectionInfo')}</button>{open && network && <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget) setOpen(false); }}><section className="modal surface modal-error-host protocol-connection-dialog" role="dialog" aria-modal="true" aria-label={t('shares.shareConnectionInfo')}><div className="modal-heading"><h2>{share.name}</h2><button className="icon-button" aria-label={t('actions.close')} onClick={() => setOpen(false)}><Icon name="close" /></button></div><dl className="detail-list"><dt>{t('shares.protocol')}</dt><dd>{protocolLabel(share.protocol, t)}</dd><dt>{t('common.status')}</dt><dd>{running ? t('shares.running') : t('shares.notRunning')}</dd><dt>{t('shares.address')}</dt><dd className="mono protocol-address">{address}</dd>{share.protocol === 'http' && share.access_mode !== 'token' && <><dt>WebDAV</dt><dd className="mono protocol-address">{webdavAddress}</dd></>}{share.protocol === 'smb' && <><dt>{t('shares.shareName')}</dt><dd>{protocolShareName}</dd></>}<dt>{t('shares.bindIp')}</dt><dd className="mono protocol-address">{share.bind_ips || '—'}</dd><dt>{t('shares.listenPort')}</dt><dd>{share.port}</dd>{share.access_mode === 'password' || share.access_mode === 'token' ? <>{share.access_mode === 'password' && <><dt>{t('shares.username')}</dt><dd>{share.access_username ?? ''}</dd></>}<dt>{share.access_mode === 'token' ? t('shares.token') : t('shares.password')}</dt><dd className="mono">{share.access_token}</dd></> : <><dt>{t('shares.accessMethod')}</dt><dd>{accessLabel(share.access_mode, t)}</dd></>}</dl><ModalErrorToast message={openError} />{share.protocol === 'http' && <div className="modal-actions"><button type="button" className="primary" onClick={() => void copyAddress()}>{t('shares.copyHttp')}</button><button type="button" className="primary" disabled={!running} onClick={() => { setOpenError(''); void openShareUrl(address).catch(error => setOpenError(errorText(error))); }}>{t('shares.openHttp')}</button></div>}</section></div>}</>;
}

function ShareLinks({ share, network, disabled = false }: { share: ShareConfig; network: ShareNetworkInfo | null; disabled?: boolean }) {
  const { t } = useTranslation('common');
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const host = network?.addresses.find(address => address.includes('.') && !address.startsWith('127.'))
    ?? network?.addresses.find(address => address.includes(':') && address !== '::1' && address !== '::')
    ?? '127.0.0.1';
  const nativeUrl = network ? `ns://node?host=${encodeURIComponent(host)}&tcp=${network.tcp_port}&quic=${network.quic_port}&node=${network.node_id}&share=${encodeURIComponent(share.id)}` : '';
  async function copy(value: string): Promise<boolean> {
    try { await navigator.clipboard.writeText(value); return true; }
    catch { return false; }
  }
  async function copyHttp() {
    setBusy(true); setMessage('');
    try {
      if (!network) return;
      const port = await startShareLink();
      const urlHost = formatUrlHost(host);
      const base = `http://${urlHost}:${port}`;
      const httpUrl = shareBrowserUrl(base, share);
      const copied = await copy(httpUrl);
      setMessage(t(copied ? 'shares.copiedHttp' : 'shares.copyFailed'));
    } catch (err) { setMessage(errorText(err)); } finally { setBusy(false); }
  }
  return <span className="share-link-actions"><button className={!network ? 'share-info-pending' : undefined} title={t('shares.copyHttpTitle')} disabled={disabled || !network || busy || !share.enabled} onClick={() => void copyHttp()}>{t('shares.copyHttp')}</button><button className={!network ? 'share-info-pending' : undefined} title={nativeUrl} disabled={disabled || !network} onClick={() => void copy(nativeUrl).then(copied => setMessage(t(copied ? 'shares.copiedNode' : 'shares.copyFailed')))}>{t('shares.copyNode')}</button>{message && <small className="caption">{message}</small>}</span>;
}

function formatSize(size: number): string { if (size < 1024) return size.toFixed(2) + ' B'; const units = ['KB', 'MB', 'GB', 'TB']; let value = size / 1024; let unit = 0; while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; } return value.toFixed(2) + ' ' + units[unit]; }

