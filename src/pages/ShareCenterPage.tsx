import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { chooseSendFiles, errorText, getNetworkStatus } from '../api/network';
import type { NetworkStatus, Peer } from '../types/network';
import { browseBrowser, chooseBrowserDownloadPath, closeBrowser, connectBrowser, createBrowserDirectory, deleteBrowserConnection, deleteBrowserEntry, downloadBrowserFile, listBrowserConnections, moveBrowserEntry, renameBrowserEntry, reopenBrowser, testBrowserConnection, updateBrowserConnection, uploadBrowserFile } from '../api/browser';
import type { BrowserConnection, BrowserConnectionInput, BrowserEntry, BrowserListing, BrowserProtocol } from '../types/browser';
import { Icon } from '../components/Icon';
import { SidebarToggle } from '../components/AppLayout';
import { SelectMenu } from '../components/SelectMenu';
import { createShare, deleteShare, listShares, rotateShareToken, startShareLink, toggleShare } from '../api/ipc';
import { shareProtocolStatus } from '../api/ipc';
import type { AccessMode, ShareConfig, ShareProtocol, ProtocolStatus } from '../types';

const labels: Record<BrowserProtocol, string> = { local: '本机共享', nodesend: 'NodeSend 原生', webdav: 'WebDAV', smb: 'SMB', ftp: 'FTP' };
const accessLabels: Record<AccessMode, string> = { open: '开放', token: '令牌', trusted: '仅信任 Node', deny: '禁止' };

export function ShareCenterPage() {
  const [connections, setConnections] = useState<BrowserConnection[]>([]);
  const [active, setActive] = useState('');
  const [listing, setListing] = useState<BrowserListing | null>(null);
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const [showConnect, setShowConnect] = useState(false);
  const [incomingShare, setIncomingShare] = useState('');
  const refresh = useCallback(async () => {
    try { const items = await listBrowserConnections(); setConnections(items); if (!active && items.find(item => item.connected)) setActive(items.find(item => item.connected)!.id); }
    catch (err) { setError(errorText(err)); }
  }, [active]);
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
    <header className="page-heading"><div><span className="eyebrow">NODESEND / FILE BROWSER</span><h1>文件浏览器</h1><p>本机和 NodeSend 节点走原生传输；第三方 WebDAV、SMB、FTP 按对方权限访问。</p></div><div className="page-heading-actions"><SidebarToggle /></div></header>
    {error && <div className="notice error" role="alert">{error}</div>}{message && <div className="notice success">{message}</div>}
    {incomingShare && <div className="notice success deep-link-notice"><span>已打开 NodeSend 分享链接。</span><a href={incomingShare} target="_blank" rel="noreferrer">打开 HTTP 分享</a><button className="text-button" onClick={() => setIncomingShare('')}>关闭</button></div>}
    <div className="browser-tabs" role="tablist">{connections.filter(connection => connection.connected).map(connection => <button key={connection.id} role="tab" aria-selected={active === connection.id} className={active === connection.id ? 'active' : ''} onClick={() => setActive(connection.id)}><span>{labels[connection.protocol]}</span><strong>{connection.name}</strong><span className="tab-close" onClick={event => { event.stopPropagation(); void disconnect(connection.id); }}>×</span></button>)}<button className="browser-tab-add" aria-label="添加连接" title="添加连接" onClick={() => setShowConnect(true)}><Icon name="plus" size={16} /></button></div>
    {active && listing ? <BrowserWorkspace listing={listing} connection={connections.find(c => c.id === active)!} onNavigate={path => load(active, path)} onRefresh={() => load(active, listing.path)} setMessage={setMessage} /> : <section className="surface empty-state generous"><Icon name="folder" size={40} /><h3>打开一个文件连接</h3><p>连接本机目录、局域网 Node 或外部协议服务。<br />每个连接都会成为一个独立标签。</p><button className="primary" onClick={() => setShowConnect(true)}>添加第一个连接</button></section>}
    <LocalShares />
    {showConnect && <ConnectDialog saved={connections} onClose={() => setShowConnect(false)} onConnected={connection => { setConnections(items => items.filter(item => item.id !== connection.id).concat(connection)); setActive(connection.id); setShowConnect(false); }} onChanged={connection => { setConnections(items => items.map(item => item.id === connection.id ? connection : item)); if (active === connection.id) { setActive(''); setListing(null); } }} onDeleted={id => { setConnections(items => items.filter(item => item.id !== id)); if (active === id) { setActive(''); setListing(null); } }} />}
  </div>;
}

function BrowserWorkspace({ listing, connection, onNavigate, onRefresh, setMessage }: { listing: BrowserListing; connection: BrowserConnection; onNavigate: (path: string) => void; onRefresh: () => void; setMessage: (message: string) => void }) {
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
    try { await downloadBrowserFile(connection.id, entry.path, destination); setMessage('文件已下载'); }
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
      setMessage(`已上传 ${files.length} 个文件`);
    } catch (err) { setMessage(errorText(err)); }
    finally { setBusy(false); }
  }
  async function mkdir() {
    const name = window.prompt('新建目录名称');
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
    if (!window.confirm('确定删除「' + entry.name + '」？')) return;
    setBusy(true);
    try { await deleteBrowserEntry(connection.id, entry.path); setSelectedPaths([]); onRefresh(); }
    catch (err) { setMessage(errorText(err)); }
    finally { setBusy(false); }
  }
  async function removeSelected() {
    if (!selected.length || !window.confirm(`确定删除选中的 ${selected.length} 项？`)) return;
    setBusy(true);
    try {
      for (const entry of selected) await deleteBrowserEntry(connection.id, entry.path);
      setSelectedPaths([]);
      onRefresh();
    } catch (err) { setMessage(errorText(err)); onRefresh(); }
    finally { setBusy(false); }
  }
  async function rename(entry: BrowserEntry) {
    const name = window.prompt('新名称', entry.name);
    if (!name || name === entry.name) return;
    setBusy(true);
    try { await renameBrowserEntry(connection.id, entry.path, name); setSelectedPaths([]); onRefresh(); }
    catch (err) { setMessage(errorText(err)); }
    finally { setBusy(false); }
  }
  async function move(entry: BrowserEntry) {
    const target = window.prompt('目标相对路径', entry.path);
    if (!target || target === entry.path) return;
    setBusy(true);
    try { await moveBrowserEntry(connection.id, entry.path, target); setSelectedPaths([]); onRefresh(); }
    catch (err) { setMessage(errorText(err)); }
    finally { setBusy(false); }
  }
  const crumbs = listing.path ? ['根目录'].concat(listing.path.split('/')) : ['根目录'];
  return <section className="surface browser-workspace">
    <div className="browser-toolbar">
      <div className="breadcrumbs">{crumbs.map((crumb, index) => <button key={crumb + index} onClick={() => { setSelectedPaths([]); onNavigate(index === 0 ? '' : crumbs.slice(1, index + 1).join('/')); }}>{crumb}</button>)}</div>
      <div className="button-row">
        <button disabled={busy || !listing.can_write} onClick={upload}>上传</button>
        {connection.protocol !== 'nodesend' && <button disabled={busy || !listing.can_write} onClick={mkdir}>新建目录</button>}
        {canManage && selected.length > 0 && <button className="danger" disabled={busy} onClick={removeSelected}>删除所选 ({selected.length})</button>}
        <button className={'refresh-button' + (busy ? ' refreshing' : '')} disabled={busy} onClick={() => void refreshListing()} aria-label="刷新目录" title="刷新目录"><Icon name="refresh" size={16} /></button>
      </div>
    </div>
    <div className="browser-location"><span className="badge">{labels[connection.protocol]}</span><span>{connection.address}</span><span className="muted">{connection.protocol === 'nodesend' ? 'QUIC/TCP 原生传输' : listing.can_write ? '按对方权限可读写' : '按对方权限只读'}</span>{busy && progress && <span>{progress}</span>}</div>
    <table className="browser-table"><thead><tr><th><input type="checkbox" aria-label="全选" checked={listing.entries.length > 0 && selected.length === listing.entries.length} onChange={event => setSelectedPaths(event.target.checked ? listing.entries.map(entry => entry.path) : [])} /></th><th>名称</th><th>修改时间</th><th>大小</th><th>操作</th></tr></thead><tbody>
      {listing.entries.map(entry => <tr key={entry.path}>
        <td><input type="checkbox" aria-label={`选择 ${entry.name}`} checked={selectedPaths.includes(entry.path)} onChange={event => setSelectedPaths(paths => event.target.checked ? [...paths, entry.path] : paths.filter(path => path !== entry.path))} /></td>
        <td><button className="browser-entry-name" onClick={() => entry.is_dir ? (setSelectedPaths([]), onNavigate(entry.path)) : setSelectedPaths([entry.path])}><Icon name={entry.is_dir ? 'folder' : 'file'} size={17} />{entry.name}</button></td>
        <td>{entry.modified || '—'}</td><td>{entry.is_dir ? '—' : formatSize(entry.size)}</td>
        <td className="actions">{!entry.is_dir && <button disabled={busy} onClick={() => void download(entry)}>下载</button>}{canManage && <><button disabled={busy} onClick={() => void rename(entry)}>重命名</button><button disabled={busy} onClick={() => void move(entry)}>移动</button><button className="danger" disabled={busy} onClick={() => void remove(entry)}>删除</button></>}</td>
      </tr>)}
      {!listing.entries.length && <tr><td colSpan={5}><div className="empty-state"><Icon name="folder" size={28} /><p>这个目录为空</p></div></td></tr>}
    </tbody></table>
    {detail && <div className="browser-detail"><strong>{detail.name}</strong><span>{detail.is_dir ? '文件夹' : '文件'}</span><span>{detail.is_dir ? '—' : formatSize(detail.size)}</span><span>{detail.modified || '修改时间未知'}</span><span className="mono">{detail.path}</span></div>}
  </section>;
}

function ConnectDialog({ saved, onClose, onConnected, onChanged, onDeleted }: { saved: BrowserConnection[]; onClose: () => void; onConnected: (connection: BrowserConnection) => void; onChanged: (connection: BrowserConnection) => void; onDeleted: (id: string) => void }) {
  const [protocol, setProtocol] = useState<BrowserProtocol>('local'); const [name, setName] = useState('本机共享'); const [address, setAddress] = useState(''); const [username, setUsername] = useState(''); const [password, setPassword] = useState(''); const [error, setError] = useState(''); const [busy, setBusy] = useState(false);
  const [details, setDetails] = useState<BrowserConnection | null>(null);
  const [peers, setPeers] = useState<Peer[]>([]);
  useEffect(() => { void getNetworkStatus().then(status => setPeers(status.peers)).catch(() => {}); }, []);
  async function submit(event: FormEvent) { event.preventDefault(); if (protocol === 'nodesend' && !address) { setError('请选择设备'); return; } setBusy(true); setError(''); const input: BrowserConnectionInput = { name: name, protocol: protocol, address: address, username: username || null, password: password || null }; try { onConnected(await connectBrowser(input)); } catch (err) { setError(errorText(err)); } finally { setBusy(false); } }
  const placeholders: Record<BrowserProtocol, string> = { local: 'D:\\Share', nodesend: '', webdav: 'https://example.com/dav/', smb: '\\\\server\\share', ftp: 'ftp://server:21/path' };
  return <><div className="modal-backdrop"><form className="modal surface" onSubmit={submit}><div className="modal-heading"><div><span className="eyebrow">NEW CONNECTION</span><h2>添加文件连接</h2></div><button type="button" className="icon-button" onClick={onClose} title="关闭"><Icon name="close" /></button></div>{saved.length > 0 && <div className="saved-connections"><h3>已保存的连接</h3>{saved.map(item => <button type="button" key={item.id} disabled={busy} onClick={() => { if (item.connected) { onConnected(item); return; } setBusy(true); setError(''); void reopenBrowser(item.id, password).then(onConnected).catch(err => setError(errorText(err))).finally(() => setBusy(false)); }} onContextMenu={event => { event.preventDefault(); setDetails(item); }}><span>{labels[item.protocol]}</span><strong>{item.name}</strong><Icon name="arrow" size={15} /></button>)}</div>}<label>协议<SelectMenu<BrowserProtocol> label="协议" value={protocol} options={[{ value: 'local', label: '本机共享' }, { value: 'nodesend', label: 'NodeSend 原生' }, { value: 'webdav', label: 'WebDAV' }, { value: 'smb', label: 'SMB' }, { value: 'ftp', label: 'FTP' }]} onChange={value => { setProtocol(value); setName(labels[value]); setAddress(''); }} disabled={busy} /></label><label>连接名称<input value={name} onChange={event => setName(event.target.value)} required /></label>{protocol === 'nodesend' ? <label>设备<SelectMenu label="设备" value={address} options={[{ value: '', label: '选择设备' }, ...peers.map(peer => ({ value: peer.node_id, label: peer.display_name + (peer.online ? '' : '（离线）') }))]} onChange={setAddress} disabled={busy} /></label> : <label>地址<input value={address} onChange={event => setAddress(event.target.value)} placeholder={placeholders[protocol]} required /></label>}{protocol !== 'local' && protocol !== 'smb' && <>{protocol !== 'nodesend' && <label>用户名（可选）<input value={username} onChange={event => setUsername(event.target.value)} /></label>}<label>{protocol === 'nodesend' ? '共享令牌（按需）' : '密码（可选）'}<input type="password" value={password} onChange={event => setPassword(event.target.value)} /></label></>}{error && <div className="notice error">{error}</div>}<div className="modal-actions"><button type="button" onClick={onClose}>取消</button><button className="primary" disabled={busy}>连接并打开</button></div></form></div>{details && <SavedConnectionDialog connection={details} onClose={() => setDetails(null)} onChanged={onChanged} onDeleted={onDeleted} />}</>;
}

function SavedConnectionDialog({ connection, onClose, onChanged, onDeleted }: { connection: BrowserConnection; onClose: () => void; onChanged: (connection: BrowserConnection) => void; onDeleted: (id: string) => void }) {
  const [name, setName] = useState(connection.name); const [protocol, setProtocol] = useState<BrowserProtocol>(connection.protocol); const [address, setAddress] = useState(connection.address); const [username, setUsername] = useState(connection.username ?? ''); const [password, setPassword] = useState(''); const [message, setMessage] = useState(''); const [busy, setBusy] = useState(false);
  const input = (): BrowserConnectionInput => ({ name, protocol, address, username: username || null, password: password || null });
  async function test() { setBusy(true); setMessage(''); try { await testBrowserConnection(input()); setMessage('连接测试成功。'); } catch (err) { setMessage(errorText(err)); } finally { setBusy(false); } }
  async function save() { setBusy(true); setMessage(''); try { onChanged(await updateBrowserConnection(connection.id, input())); setMessage('连接信息已保存。'); } catch (err) { setMessage(errorText(err)); } finally { setBusy(false); } }
  async function remove() { setBusy(true); try { await deleteBrowserConnection(connection.id); onDeleted(connection.id); onClose(); } catch (err) { setMessage(errorText(err)); } finally { setBusy(false); } }
  return <div className="modal-backdrop"><section className="modal surface" role="dialog" aria-modal="true" onKeyDown={event => { if (event.key !== 'Enter') return; const target = event.target as HTMLElement; if (target.tagName === 'INPUT') { const inputs = Array.from(event.currentTarget.querySelectorAll<HTMLInputElement>('input')); const index = inputs.indexOf(target as HTMLInputElement); if ((target as HTMLInputElement).type === 'password') { event.preventDefault(); target.blur(); } else if (index >= 0 && index < inputs.length - 1) { event.preventDefault(); inputs[index + 1].focus(); } } else { event.preventDefault(); void save(); } }}><div className="modal-heading"><div><span className="eyebrow">SAVED CONNECTION</span><h2>连接详情</h2></div><button type="button" className="icon-button" onClick={onClose} aria-label="关闭"><Icon name="close" /></button></div><label>协议<SelectMenu<BrowserProtocol> label="协议" value={protocol} options={[{ value: 'local', label: '本机共享' }, { value: 'nodesend', label: 'NodeSend 原生' }, { value: 'webdav', label: 'WebDAV' }, { value: 'smb', label: 'SMB' }, { value: 'ftp', label: 'FTP' }]} onChange={setProtocol} disabled={busy} /></label><label>连接名称<input value={name} onChange={event => setName(event.target.value)} /></label><label>地址<input value={address} onChange={event => setAddress(event.target.value)} /></label><label>用户名（可选）<input value={username} onChange={event => setUsername(event.target.value)} /></label><label>密码（留空则保留当前会话密码）<input type="password" value={password} onChange={event => setPassword(event.target.value)} /></label>{message && <div className="notice" role="status">{message}</div>}<div className="modal-actions"><button type="button" className="danger" disabled={busy} onClick={() => void remove()}>删除</button><button type="button" disabled={busy} onClick={() => void test()}>测试连接</button><button type="button" className="primary" disabled={busy} onClick={() => void save()}>保存修改</button></div></section></div>;
}

function LocalShares() {
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
      setMessage(accessMode === 'deny' ? '共享目录已保存，当前禁止访问。' : '共享目录已添加，服务已启动。');
    });
  }
  const accessOptions: { value: AccessMode; label: string }[] = [
    { value: 'token', label: shareProtocol === 'nodesend' ? '令牌访问' : '密码访问' },
    { value: 'open', label: shareProtocol === 'nodesend' ? '开放访问' : '匿名访问' },
    ...(shareProtocol === 'nodesend' ? [{ value: 'trusted' as const, label: '仅信任 Node' }] : []),
    { value: 'deny', label: '禁止访问' },
  ];
  return <section className="surface local-shares">
    <div className="section-title"><h2>本机共享目录</h2></div>
    <form className="share-form" onSubmit={create}>
      <input name="name" aria-label="共享名称" placeholder="共享名称" required disabled={busy} />
      <input name="path" aria-label="共享目录" placeholder="共享目录完整路径" required disabled={busy} />
      <SelectMenu label="共享访问模式" value={accessMode} options={accessOptions} onChange={setAccessMode} disabled={busy} />
      <SelectMenu label="共享协议" value={shareProtocol} options={[{ value: 'nodesend', label: 'NodeSend' }, { value: 'webdav', label: 'WebDAV' }, { value: 'smb', label: 'SMB' }, { value: 'ftp', label: 'FTP' }]} onChange={protocol => { setShareProtocol(protocol); if (accessMode === 'trusted') setAccessMode('token'); setPort(protocol === 'smb' ? '445' : '0'); }} disabled={busy} />
      {shareProtocol !== 'nodesend' && <input className="share-port" type="number" min="0" max="65535" aria-label="监听端口，0为自动分配" value={port} onChange={event => setPort(event.target.value)} required disabled={busy} />}
      <button className="primary" disabled={busy}>添加目录</button>
    </form>
    {shareProtocol !== 'nodesend' && <p className="caption">端口填写 0 自动分配。密码访问的用户名为 nodesend，密码在创建后查看。</p>}
    {shareProtocol === 'smb' && <p className="caption">Windows 资源管理器通常使用 445 端口；若系统共享已占用，可改用其他端口并使用支持指定端口的 SMB 客户端。</p>}
    {shareProtocol === 'ftp' && <p className="caption">FTP 被动数据端口：50000–51000。FTP 和 HTTP WebDAV 适合受信任的局域网。</p>}
    {message && <p className="hint" role="status">{message}</p>}
    <div className="share-table-wrap"><table className="table"><thead><tr><th>名称</th><th>路径</th><th>协议</th><th>访问模式</th><th>状态</th><th>操作</th></tr></thead>
      <tbody>{shares.map(share => {
        const service = services.find(item => item.id === share.id);
        const native = share.protocol === 'nodesend';
        const running = native ? share.enabled : !!service?.running;
        return <tr key={share.id}><td>{share.name}</td><td className="mono">{share.path}</td><td>{labels[share.protocol]}</td>
          <td>{share.access_mode === 'token' && !native ? '密码' : accessLabels[share.access_mode]}</td>
          <td>{!share.enabled ? '已停用' : share.access_mode === 'deny' ? '禁止访问' : running ? '已启用' : '未启动'}{service?.error && <small className="share-service-error">{service.error}</small>}</td>
          <td className="actions"><button disabled={busy} onClick={() => void perform(() => toggleShare(share.id, !running))}>{running ? '停用' : '启用'}</button>
            {share.access_mode === 'token' && <button disabled={busy} onClick={() => void perform(() => rotateShareToken(share.id))}>{native ? '轮换令牌' : '重置密码'}</button>}
            {network && (native ? <ShareLinks share={share} network={network} /> : <ProtocolConnection share={share} network={network} running={running} />)}
            <button disabled={busy} className="danger" onClick={() => void perform(() => deleteShare(share.id))}>删除</button>
          </td></tr>;
      })}</tbody></table></div>
  </section>;
}

function ProtocolConnection({ share, network, running }: { share: ShareConfig; network: NetworkStatus; running: boolean }) {
  const [open, setOpen] = useState(false);
  const host = network.addresses.find(address => address.includes('.') && !address.startsWith('127.')) ?? '127.0.0.1';
  const address = share.protocol === 'webdav' ? `http://${host}:${share.port}/` : share.protocol === 'ftp' ? `ftp://${host}:${share.port}/` : `smb://${host}:${share.port}/share`;
  return <><button onClick={() => setOpen(true)}>连接信息</button>{open && <div className="modal-backdrop"><section className="modal surface" role="dialog" aria-modal="true" aria-label="共享连接信息"><div className="modal-heading"><h2>{share.name}</h2><button className="icon-button" aria-label="关闭" onClick={() => setOpen(false)}><Icon name="close" /></button></div><dl className="detail-list"><dt>协议</dt><dd>{labels[share.protocol]}</dd><dt>状态</dt><dd>{running ? '运行中' : '未运行'}</dd><dt>地址</dt><dd className="mono">{address}</dd><dt>端口</dt><dd>{share.port}</dd>{share.protocol === 'smb' && <><dt>共享名称</dt><dd>share</dd></>}{share.access_mode === 'token' ? <><dt>用户名</dt><dd>nodesend</dd><dt>密码</dt><dd className="mono">{share.access_token}</dd></> : <><dt>访问方式</dt><dd>{accessLabels[share.access_mode]}</dd></>}</dl><div className="modal-actions"><button className="primary" onClick={() => setOpen(false)}>关闭</button></div></section></div>}</>;
}

function ShareLinks({ share, network }: { share: ShareConfig; network: NetworkStatus }) {
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
      await copy(httpUrl); setMessage('HTTP 链接已复制');
    } catch (err) { setMessage(errorText(err)); } finally { setBusy(false); }
  }
  return <span className="share-link-actions"><button title="启动独立链接分享 HTTP 服务并复制" disabled={busy || !share.enabled} onClick={() => void copyHttp()}>复制 HTTP 链接</button><button title={nativeUrl} onClick={() => void copy(nativeUrl)}>复制 NodeSend 链接</button>{message && <small className="caption">{message}</small>}</span>;
}

function formatSize(size: number): string { if (size < 1024) return size.toFixed(2) + ' B'; const units = ['KB', 'MB', 'GB', 'TB']; let value = size / 1024; let unit = 0; while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; } return value.toFixed(2) + ' ' + units[unit]; }

