import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { chooseSendFiles, errorText, getNetworkStatus } from '../api/network';
import type { NetworkStatus, Peer } from '../types/network';
import { browseBrowser, chooseBrowserDownloadPath, closeBrowser, connectBrowser, createBrowserDirectory, deleteBrowserConnection, deleteBrowserEntry, downloadBrowserFile, listBrowserConnections, moveBrowserEntry, renameBrowserEntry, reopenBrowser, testBrowserConnection, updateBrowserConnection, uploadBrowserFile } from '../api/browser';
import type { BrowserConnection, BrowserConnectionInput, BrowserEntry, BrowserListing, BrowserProtocol } from '../types/browser';
import { Icon } from '../components/Icon';
import { SidebarToggle, WindowControls } from '../components/AppLayout';
import { SelectMenu } from '../components/SelectMenu';
import { createShare, deleteShare, listShares, rotateShareToken, startShareLink, toggleShare } from '../api/ipc';
import { shareProtocolStatus } from '../api/ipc';
import type { AccessMode, ShareConfig, ShareProtocol, ProtocolStatus } from '../types';
import { useTranslation } from 'react-i18next';

function protocolLabel(protocol: BrowserProtocol, t: (key: string) => string) { return protocol === 'local' ? t('shares.local') : ({ nodesend: 'NodeSend', webdav: 'WebDAV', smb: 'SMB', ftp: 'FTP' } as const)[protocol]; }
function accessLabel(mode: AccessMode, t: (key: string) => string) { return t(`shares.${mode}`); }

export function ShareCenterPage() {
  const { t } = useTranslation('common');
  const [connections, setConnections] = useState<BrowserConnection[]>([]);
  const [active, setActive] = useState('');
  const [listing, setListing] = useState<BrowserListing | null>(null);
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const [showConnect, setShowConnect] = useState(false);
  const [incomingShare, setIncomingShare] = useState('');
  const refresh = useCallback(async () => {
    try {
      const items = await listBrowserConnections();
      setConnections(items);
      const firstConnected = items.find(item => item.connected);
      if (firstConnected) setActive(current => current || firstConnected.id);
    }
    catch (err) { setError(errorText(err)); }
  }, []);
  useEffect(() => { void refresh(); }, [refresh]);
  useEffect(() => { if (active) void load(active, ''); }, [active]);
  useEffect(() => {
    const onDeepLink = (event: Event) => {
      const detail = (event as CustomEvent<{ kind?: string; httpUrl?: string }>).detail;
      if (detail.kind === 'share' && detail.httpUrl) setIncomingShare(detail.httpUrl);
    };
    window.addEventListener('nodesend-deep-link', onDeepLink);
    return () => window.removeEventListener('nodesend-deep-link', onDeepLink);
  }, []);
  async function load(id: string, path: string) { setError(''); try { setListing(await browseBrowser(id, path)); } catch (err) { setListing(null); setError(errorText(err)); } }
  async function disconnect(id: string) { await closeBrowser(id); if (active === id) { setActive(''); setListing(null); } await refresh(); }
  return <div className="page browser-page">
    <header className="page-heading"><div><span className="eyebrow">NODESEND / FILE BROWSER</span></div><div className="page-heading-actions"><SidebarToggle /><WindowControls /></div></header>
    {error && <div className="notice error" role="alert">{error}</div>}{message && <div className="notice success">{message}</div>}
    {incomingShare && <div className="notice success deep-link-notice"><span>{t('shares.openedLink')}</span><a href={incomingShare} target="_blank" rel="noreferrer">{t('shares.openHttp')}</a><button className="text-button" onClick={() => setIncomingShare('')}>{t('actions.close')}</button></div>}
    <div className="browser-tabs" role="tablist">{connections.filter(connection => connection.connected).map(connection => <button key={connection.id} role="tab" aria-selected={active === connection.id} className={active === connection.id ? 'active' : ''} onClick={() => setActive(connection.id)}><span>{protocolLabel(connection.protocol, t)}</span><strong>{connection.name}</strong><span className="tab-close" onClick={event => { event.stopPropagation(); void disconnect(connection.id); }}>×</span></button>)}<button className="browser-tab-add" aria-label={t('shares.addConnection')} title={t('shares.addConnection')} onClick={() => setShowConnect(true)}><Icon name="plus" size={16} /></button></div>
    {active && listing ? <BrowserWorkspace listing={listing} connection={connections.find(c => c.id === active)!} onNavigate={path => load(active, path)} onRefresh={() => load(active, listing.path)} setMessage={setMessage} /> : <section className="surface empty-state generous"><Icon name="folder" size={40} /><h3>{t('shares.openConnection')}</h3></section>}
    <LocalShares />
    {showConnect && <ConnectDialog saved={connections} onClose={() => setShowConnect(false)} onConnected={connection => { setConnections(items => items.filter(item => item.id !== connection.id).concat(connection)); setActive(connection.id); setShowConnect(false); }} onChanged={connection => { setConnections(items => items.map(item => item.id === connection.id ? connection : item)); if (active === connection.id) { setActive(''); setListing(null); } }} onDeleted={id => { setConnections(items => items.filter(item => item.id !== id)); if (active === id) { setActive(''); setListing(null); } }} />}
  </div>;
}

function BrowserWorkspace({ listing, connection, onNavigate, onRefresh, setMessage }: { listing: BrowserListing; connection: BrowserConnection; onNavigate: (path: string) => void; onRefresh: () => void; setMessage: (message: string) => void }) {
  const { t } = useTranslation('common');
  const [busy, setBusy] = useState(false);
  const [selectedPaths, setSelectedPaths] = useState<string[]>([]);
  const [progress, setProgress] = useState('');
  const selected = listing.entries.filter(entry => selectedPaths.includes(entry.path));
  const detail = selected.length === 1 ? selected[0] : null;
  const canManage = listing.can_write && connection.protocol !== 'nodesend';

  async function download(entry: BrowserEntry) {
    const destination = await chooseBrowserDownloadPath(entry.name);
    if (!destination) return;
    setBusy(true);
    try { await downloadBrowserFile(connection.id, entry.path, destination); setMessage(t('shares.downloaded')); }
    catch (err) { setMessage(errorText(err)); }
    finally { setBusy(false); }
  }
  async function upload() {
    const files = await chooseSendFiles(false);
    if (!files.length) return;
    setBusy(true);
    setProgress('');
    try {
      for (let i = 0; i < files.length; i++) {
        const name = files[i].split(/[\\/]/).pop() || 'file';
        await uploadBrowserFile(connection.id, files[i], listing.path ? listing.path + '/' + name : name);
        setProgress(`${i + 1} / ${files.length}`);
      }
      onRefresh();
      setMessage(t('shares.uploaded', { count: files.length }));
    } catch (err) { setMessage(errorText(err)); }
    finally { setBusy(false); }
  }
  async function mkdir() {
    const name = window.prompt(t('shares.newDirectory'));
    if (!name) return;
    setBusy(true);
    try { await createBrowserDirectory(connection.id, listing.path ? listing.path + '/' + name : name); onRefresh(); }
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
    try { await deleteBrowserEntry(connection.id, entry.path); setSelectedPaths([]); onRefresh(); }
    catch (err) { setMessage(errorText(err)); }
    finally { setBusy(false); }
  }
  async function removeSelected() {
    if (!selected.length || !window.confirm(t('shares.confirmDeleteSelected', { count: selected.length }))) return;
    setBusy(true);
    try {
      for (const entry of selected) await deleteBrowserEntry(connection.id, entry.path);
      setSelectedPaths([]);
      onRefresh();
    } catch (err) { setMessage(errorText(err)); onRefresh(); }
    finally { setBusy(false); }
  }
  async function rename(entry: BrowserEntry) {
    const name = window.prompt(t('shares.newName'), entry.name);
    if (!name || name === entry.name) return;
    setBusy(true);
    try { await renameBrowserEntry(connection.id, entry.path, name); setSelectedPaths([]); onRefresh(); }
    catch (err) { setMessage(errorText(err)); }
    finally { setBusy(false); }
  }
  async function move(entry: BrowserEntry) {
    const target = window.prompt(t('shares.targetPath'), entry.path);
    if (!target || target === entry.path) return;
    setBusy(true);
    try { await moveBrowserEntry(connection.id, entry.path, target); setSelectedPaths([]); onRefresh(); }
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

function ConnectDialog({ saved, onClose, onConnected, onChanged, onDeleted }: { saved: BrowserConnection[]; onClose: () => void; onConnected: (connection: BrowserConnection) => void; onChanged: (connection: BrowserConnection) => void; onDeleted: (id: string) => void }) {
  const { t } = useTranslation('common');
  const [protocol, setProtocol] = useState<BrowserProtocol>('nodesend'); const [name, setName] = useState('NodeSend'); const [address, setAddress] = useState(''); const [username, setUsername] = useState(''); const [password, setPassword] = useState(''); const [error, setError] = useState(''); const [busy, setBusy] = useState(false);
  const [details, setDetails] = useState<BrowserConnection | null>(null);
  const [peers, setPeers] = useState<Peer[]>([]);
  useEffect(() => { void getNetworkStatus().then(status => setPeers(status.peers)).catch(() => {}); }, []);
  async function submit(event: FormEvent) { event.preventDefault(); if (protocol === 'nodesend' && !address) { setError(t('shares.chooseDevice')); return; } setBusy(true); setError(''); const input: BrowserConnectionInput = { name: name, protocol: protocol, address: address, username: username || null, password: password || null }; try { onConnected(await connectBrowser(input)); } catch (err) { setError(errorText(err)); } finally { setBusy(false); } }
  const placeholders: Record<BrowserProtocol, string> = { local: 'D:\\Share', nodesend: '', webdav: 'https://example.com/dav/', smb: '\\\\server\\share', ftp: 'ftp://server:21/path' };
  return <><div className="modal-backdrop"><form className="modal surface" onSubmit={submit}><div className="modal-heading"><div><span className="eyebrow">NEW CONNECTION</span><h2>{t('shares.newConnection')}</h2></div><button type="button" className="icon-button" onClick={onClose} title={t('actions.close')}><Icon name="close" /></button></div>{saved.length > 0 && <div className="saved-connections"><h3>{t('shares.savedConnections')}</h3>{saved.map(item => <button type="button" key={item.id} disabled={busy} onClick={() => { if (item.connected) { onConnected(item); return; } setBusy(true); setError(''); void reopenBrowser(item.id, password).then(onConnected).catch(err => setError(errorText(err))).finally(() => setBusy(false)); }} onContextMenu={event => { event.preventDefault(); setDetails(item); }}><span>{protocolLabel(item.protocol, t)}</span><strong>{item.name}</strong><Icon name="arrow" size={15} /></button>)}</div>}<label>{t('shares.protocol')}<SelectMenu<BrowserProtocol> label={t('shares.protocol')} value={protocol} options={[{ value: 'nodesend', label: 'NodeSend' }, { value: 'webdav', label: 'WebDAV' }, { value: 'smb', label: 'SMB' }, { value: 'ftp', label: 'FTP' }]} onChange={value => { setProtocol(value); setName(protocolLabel(value, t)); setAddress(''); }} disabled={busy} /></label><label>{t('shares.connectionName')}<input value={name} onChange={event => setName(event.target.value)} required /></label>{protocol === 'nodesend' ? <label>{t('shares.device')}<SelectMenu label={t('shares.device')} value={address} options={[{ value: '', label: t('shares.selectDevice') }, ...peers.map(peer => ({ value: peer.node_id, label: peer.display_name + (peer.online ? '' : t('shares.offline')) }))]} onChange={setAddress} disabled={busy} /></label> : <label>{t('shares.address')}<input value={address} onChange={event => setAddress(event.target.value)} placeholder={placeholders[protocol]} required /></label>}{protocol !== 'local' && protocol !== 'smb' && <>{protocol !== 'nodesend' && <label>{t('shares.usernameOptional')}<input value={username} onChange={event => setUsername(event.target.value)} /></label>}<label>{protocol === 'nodesend' ? t('shares.sharedToken') : t('shares.passwordOptional')}<input type="password" value={password} onChange={event => setPassword(event.target.value)} /></label></>}{error && <div className="notice error">{error}</div>}<div className="modal-actions"><button type="button" onClick={onClose}>{t('actions.cancel')}</button><button className="primary" disabled={busy}>{t('shares.connectOpen')}</button></div></form></div>{details && <SavedConnectionDialog connection={details} onClose={() => setDetails(null)} onChanged={onChanged} onDeleted={onDeleted} />}</>;
}

function SavedConnectionDialog({ connection, onClose, onChanged, onDeleted }: { connection: BrowserConnection; onClose: () => void; onChanged: (connection: BrowserConnection) => void; onDeleted: (id: string) => void }) {
  const { t } = useTranslation('common');
  const [name, setName] = useState(connection.name); const [protocol, setProtocol] = useState<BrowserProtocol>(connection.protocol); const [address, setAddress] = useState(connection.address); const [username, setUsername] = useState(connection.username ?? ''); const [password, setPassword] = useState(''); const [message, setMessage] = useState(''); const [busy, setBusy] = useState(false);
  const input = (): BrowserConnectionInput => ({ name, protocol, address, username: username || null, password: password || null });
  async function test() { setBusy(true); setMessage(''); try { await testBrowserConnection(input()); setMessage(t('shares.testSuccess')); } catch (err) { setMessage(errorText(err)); } finally { setBusy(false); } }
  async function save() { setBusy(true); setMessage(''); try { onChanged(await updateBrowserConnection(connection.id, input())); setMessage(t('shares.saved')); } catch (err) { setMessage(errorText(err)); } finally { setBusy(false); } }
  async function remove() { setBusy(true); try { await deleteBrowserConnection(connection.id); onDeleted(connection.id); onClose(); } catch (err) { setMessage(errorText(err)); } finally { setBusy(false); } }
  return <div className="modal-backdrop"><section className="modal surface" role="dialog" aria-modal="true" onKeyDown={event => { if (event.key !== 'Enter') return; const target = event.target as HTMLElement; if (target.tagName === 'INPUT') { const inputs = Array.from(event.currentTarget.querySelectorAll<HTMLInputElement>('input')); const index = inputs.indexOf(target as HTMLInputElement); if ((target as HTMLInputElement).type === 'password') { event.preventDefault(); target.blur(); } else if (index >= 0 && index < inputs.length - 1) { event.preventDefault(); inputs[index + 1].focus(); } } else { event.preventDefault(); void save(); } }}><div className="modal-heading"><div><span className="eyebrow">SAVED CONNECTION</span><h2>{t('shares.details')}</h2></div><button type="button" className="icon-button" onClick={onClose} aria-label={t('actions.close')}><Icon name="close" /></button></div><label>{t('shares.protocol')}<SelectMenu<BrowserProtocol> label={t('shares.protocol')} value={protocol} options={[{ value: 'nodesend', label: 'NodeSend' }, { value: 'webdav', label: 'WebDAV' }, { value: 'smb', label: 'SMB' }, { value: 'ftp', label: 'FTP' }]} onChange={setProtocol} disabled={busy} /></label><label>{t('shares.connectionName')}<input value={name} onChange={event => setName(event.target.value)} /></label><label>{t('shares.address')}<input value={address} onChange={event => setAddress(event.target.value)} /></label><label>{t('shares.usernameOptional')}<input value={username} onChange={event => setUsername(event.target.value)} /></label><label>{t('shares.passwordKeep')}<input type="password" value={password} onChange={event => setPassword(event.target.value)} /></label>{message && <div className="notice" role="status">{message}</div>}<div className="modal-actions"><button type="button" className="danger" disabled={busy} onClick={() => void remove()}>{t('shares.delete')}</button><button type="button" disabled={busy} onClick={() => void test()}>{t('shares.testConnection')}</button><button type="button" className="primary" disabled={busy} onClick={() => void save()}>{t('shares.saveChanges')}</button></div></section></div>;
}

function LocalShares() {
  const { t } = useTranslation('common');
  const [shares, setShares] = useState<ShareConfig[]>([]);
  const [services, setServices] = useState<ProtocolStatus[]>([]);
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(false);
  const [accessMode, setAccessMode] = useState<AccessMode>('token');
  const [shareProtocol, setShareProtocol] = useState<ShareProtocol>('nodesend');
  const [port, setPort] = useState('0');
  const [network, setNetwork] = useState<NetworkStatus | null>(null);
  const refresh = useCallback(async () => {
    const [items, statuses] = await Promise.all([listShares(), shareProtocolStatus()]);
    setShares(items); setServices(statuses);
  }, []);
  useEffect(() => { void refresh().catch(err => setMessage(errorText(err))); const timer = window.setInterval(() => void refresh().catch(() => {}), 3000); return () => window.clearInterval(timer); }, [refresh]);
  useEffect(() => { void getNetworkStatus().then(setNetwork).catch(() => {}); }, []);
  async function perform(action: () => Promise<unknown>) {
    setBusy(true); setMessage('');
    try { await action(); } catch (err) { setMessage(errorText(err)); }
    finally { await refresh().catch(err => setMessage(errorText(err))); setBusy(false); }
  }
  async function create(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const element = event.currentTarget;
    const form = new FormData(element);
    await perform(async () => {
      await createShare({ name: String(form.get('name')), path: String(form.get('path')), access_mode: accessMode, protocol: shareProtocol, port: shareProtocol === 'nodesend' ? 0 : Number(port) });
      element.reset();
      setMessage(accessMode === 'deny' ? t('shares.savedShare') : t('shares.shareAdded'));
    });
  }
  const accessOptions: { value: AccessMode; label: string }[] = [
    { value: 'token', label: shareProtocol === 'nodesend' ? t('shares.token') : t('shares.password') },
    { value: 'open', label: t('shares.open') },
    ...(shareProtocol === 'nodesend' ? [{ value: 'trusted' as const, label: t('shares.trusted') }] : []),
    { value: 'deny', label: t('shares.deny') },
  ];
  return <section className="surface local-shares">
    <div className="section-title"><h2>{t('shares.localShares')}</h2></div>
    <form className="share-form" onSubmit={create}>
      <input name="name" aria-label={t('shares.shareName')} placeholder={t('shares.shareName')} required disabled={busy} />
      <input name="path" aria-label={t('shares.sharePath')} placeholder={t('shares.sharePath')} required disabled={busy} />
      <SelectMenu label={t('shares.accessMode')} value={accessMode} options={accessOptions} onChange={setAccessMode} disabled={busy} />
      <SelectMenu label={t('shares.shareProtocol')} value={shareProtocol} options={[{ value: 'nodesend', label: 'NodeSend' }, { value: 'webdav', label: 'WebDAV' }, { value: 'smb', label: 'SMB' }, { value: 'ftp', label: 'FTP' }]} onChange={protocol => { setShareProtocol(protocol); if (accessMode === 'trusted') setAccessMode('token'); setPort(protocol === 'smb' ? '445' : '0'); }} disabled={busy} />
      {shareProtocol !== 'nodesend' && <input className="share-port" type="number" min="0" max="65535" aria-label={t('shares.listenPort')} value={port} onChange={event => setPort(event.target.value)} required disabled={busy} />}
      <button className="primary" disabled={busy}>{t('shares.addFolder')}</button>
    </form>
    {shareProtocol !== 'nodesend' && <p className="caption">{t('shares.portHint')}</p>}
    {shareProtocol === 'smb' && <p className="caption">{t('shares.smbHint')}</p>}
    {shareProtocol === 'ftp' && <p className="caption">{t('shares.ftpHint')}</p>}
    {message && <p className="hint" role="status">{message}</p>}
    <div className="share-table-wrap"><table className="table"><thead><tr><th>{t('shares.name')}</th><th>{t('common.path')}</th><th>{t('shares.protocol')}</th><th>{t('shares.accessMode')}</th><th>{t('common.status')}</th><th>{t('shares.actions')}</th></tr></thead>
      <tbody>{shares.map(share => {
        const service = services.find(item => item.id === share.id);
        const native = share.protocol === 'nodesend';
        const running = native ? share.enabled : !!service?.running;
        return <tr key={share.id}><td>{share.name}</td><td className="mono">{share.path}</td><td>{protocolLabel(share.protocol, t)}</td>
          <td>{share.access_mode === 'token' && !native ? t('shares.password') : accessLabel(share.access_mode, t)}</td>
          <td>{!share.enabled ? t('shares.disabled') : share.access_mode === 'deny' ? t('shares.deny') : running ? t('shares.enabled') : t('shares.notStarted')}{service?.error && <small className="share-service-error">{errorText(service.error)}</small>}</td>
          <td className="actions"><button disabled={busy} onClick={() => void perform(() => toggleShare(share.id, !running))}>{running ? t('shares.stop') : t('shares.start')}</button>
            {share.access_mode === 'token' && <button disabled={busy} onClick={() => void perform(() => rotateShareToken(share.id))}>{native ? t('shares.rotateToken') : t('shares.resetPassword')}</button>}
            {network && (native ? <ShareLinks share={share} network={network} /> : <ProtocolConnection share={share} network={network} running={running} />)}
            <button disabled={busy} className="danger" onClick={() => void perform(() => deleteShare(share.id))}>{t('shares.delete')}</button>
          </td></tr>;
      })}</tbody></table></div>
  </section>;
}

function ProtocolConnection({ share, network, running }: { share: ShareConfig; network: NetworkStatus; running: boolean }) {
  const { t } = useTranslation('common');
  const [open, setOpen] = useState(false);
  const host = network.addresses.find(address => address.includes('.') && !address.startsWith('127.')) ?? '127.0.0.1';
  const address = share.protocol === 'webdav' ? `http://${host}:${share.port}/` : share.protocol === 'ftp' ? `ftp://${host}:${share.port}/` : `smb://${host}:${share.port}/share`;
  return <><button onClick={() => setOpen(true)}>{t('shares.connectionInfo')}</button>{open && <div className="modal-backdrop"><section className="modal surface" role="dialog" aria-modal="true" aria-label={t('shares.shareConnectionInfo')}><div className="modal-heading"><h2>{share.name}</h2><button className="icon-button" aria-label={t('actions.close')} onClick={() => setOpen(false)}><Icon name="close" /></button></div><dl className="detail-list"><dt>{t('shares.protocol')}</dt><dd>{protocolLabel(share.protocol, t)}</dd><dt>{t('common.status')}</dt><dd>{running ? t('shares.running') : t('shares.notRunning')}</dd><dt>{t('shares.address')}</dt><dd className="mono">{address}</dd><dt>{t('shares.listenPort')}</dt><dd>{share.port}</dd>{share.protocol === 'smb' && <><dt>{t('shares.shareName')}</dt><dd>share</dd></>}{share.access_mode === 'token' ? <><dt>{t('shares.username')}</dt><dd>nodesend</dd><dt>{t('shares.password')}</dt><dd className="mono">{share.access_token}</dd></> : <><dt>{t('shares.accessMethod')}</dt><dd>{accessLabel(share.access_mode, t)}</dd></>}</dl><div className="modal-actions"><button className="primary" onClick={() => setOpen(false)}>{t('actions.close')}</button></div></section></div>}</>;
}

function ShareLinks({ share, network }: { share: ShareConfig; network: NetworkStatus }) {
  const { t } = useTranslation('common');
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const host = network.addresses.find(address => address.includes('.') && !address.startsWith('127.'))
    ?? network.addresses.find(address => address !== '::1')
    ?? '127.0.0.1';
  const token = share.access_mode === 'token' && share.access_token ? `&access_token=${encodeURIComponent(share.access_token)}` : '';
  const nativeUrl = `ns://node?host=${encodeURIComponent(host)}&tcp=${network.tcp_port}&quic=${network.quic_port}&node=${network.node_id}&share=${encodeURIComponent(share.id)}`;
  async function copy(value: string) { try { await navigator.clipboard.writeText(value); } catch { /* clipboard may be unavailable in preview */ } }
  async function copyHttp() {
    setBusy(true); setMessage('');
    try {
      const port = await startShareLink();
      const httpUrl = `http://${host}:${port}/api/v1/shares/${encodeURIComponent(share.id)}/tree?path=${token}`;
      await copy(httpUrl); setMessage(t('shares.copiedHttp'));
    } catch (err) { setMessage(errorText(err)); } finally { setBusy(false); }
  }
  return <span className="share-link-actions"><button title={t('shares.copyHttpTitle')} disabled={busy || !share.enabled} onClick={() => void copyHttp()}>{t('shares.copyHttp')}</button><button title={nativeUrl} onClick={() => void copy(nativeUrl)}>{t('shares.copyNode')}</button>{message && <small className="caption">{message}</small>}</span>;
}

function formatSize(size: number): string { if (size < 1024) return size.toFixed(2) + ' B'; const units = ['KB', 'MB', 'GB', 'TB']; let value = size / 1024; let unit = 0; while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; } return value.toFixed(2) + ' ' + units[unit]; }

