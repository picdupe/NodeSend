import { useReceiveDirectoryLabel } from '../../hooks/useReceiveDirectoryLabel';
import { useEffect, useRef, useState } from 'react';
import type { ConflictPolicy, NetworkStatus, TransferTask } from '../../types/network';
import { chooseReceiveDirectory, decideIncoming, errorText } from '../../api/network';
import { Icon } from '../Icon';
import { formatBytes } from './format';
function RequestCard({ task, directory, refresh }: { task: TransferTask; directory: string; refresh: () => Promise<void> }) {
  const [destination, setDestination] = useState(directory);
  const manuallySelected = useRef(false);
  useEffect(() => { if (!manuallySelected.current) setDestination(directory); }, [directory]);
  const destinationLabel = useReceiveDirectoryLabel(destination);
  const [policy, setPolicy] = useState<ConflictPolicy>('rename');
  const [selected, setSelected] = useState(task.entries.filter(e => !e.directory || !task.entries.some(c => c.path.startsWith(e.path + '/'))).map(e => e.path));
  const [trustDevice, setTrustDevice] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  async function choose() { try { const path = await chooseReceiveDirectory(); if (path) { manuallySelected.current = true; setDestination(path); } } catch (e) { setError(errorText(e)); } }
  async function decide(accepted: boolean) { setBusy(true); try { await decideIncoming(task.id, accepted, destination, policy, selected, accepted && trustDevice); await refresh(); } catch (e) { setError(errorText(e)); } finally { setBusy(false); } }
  return <article className="surface request-card">
    <div className="request-heading"><span className="device-icon"><Icon name="receive" /></span><div><h3>{task.peer_name} 想发送文件</h3><p>{task.entries.filter(e => !e.directory).length} 个文件 · {formatBytes(task.total_bytes)} · 身份已验证</p></div></div>
    <p className="caption mono">Node ID：{task.peer_id}</p>
    <details><summary>查看并选择接收内容</summary><div className="request-files">{task.entries.filter(e => !e.directory || !task.entries.some(c => c.path.startsWith(e.path + '/'))).map(e => <label className="check-label" key={e.path}><input type="checkbox" checked={selected.includes(e.path)} onChange={event => setSelected(old => event.target.checked ? [...old, e.path] : old.filter(p => p !== e.path))} /><span className="check-mark" aria-hidden="true"><Icon name="check" size={14} /></span><span>{e.path}</span><small>{e.directory ? '空目录' : formatBytes(e.size)}</small></label>)}</div></details>
    <div className="receive-options"><div><label>保存到</label><button className="folder-button" onClick={choose} title={destinationLabel}><Icon name="folder" size={16} /><span>{destinationLabel}</span><span>更改</span></button></div><label>同名文件<select value={policy} onChange={e => setPolicy(e.target.value as ConflictPolicy)}><option value="rename">保留两份（重命名）</option><option value="skip">跳过已有文件</option><option value="overwrite">覆盖同路径文件</option></select></label></div>
    {policy === 'overwrite' && <p className="caption">确认接收将覆盖所选目录中的同路径文件。</p>}
    <label className="check-label"><input type="checkbox" checked={trustDevice} disabled={busy} onChange={event => setTrustDevice(event.target.checked)} /><span className="check-mark" aria-hidden="true"><Icon name="check" size={14} /></span><span>信任此设备，以后自动接收（自动拒绝策略除外）</span></label>
    {error && <div className="notice error" role="alert">{error}</div>}
    <div className="request-actions"><button className="danger" disabled={busy} onClick={() => decide(false)}>拒绝</button><button className="primary" disabled={busy || !selected.length} onClick={() => decide(true)}><Icon name="check" size={17} />确认接收</button></div>
  </article>;
}
export function ReceivePanel({ status, refresh }: { status: NetworkStatus; refresh: () => Promise<void> }) {
  const requests = status.tasks.filter(t => t.direction === 'receive' && t.status === 'waiting');
  const [copied, setCopied] = useState(false);
  return <>
    <section className="receive-identity surface"><div className="identity-mark"><Icon name="device" size={34} /></div><div><span className="eyebrow">这台设备</span><h2>{status.display_name}</h2><p>告诉发送方选择这个名称</p><span className="badge success"><span className="status-dot online" />等待来自其他 Node 的请求</span></div><button className="copy-node-id" onClick={async () => { try { await navigator.clipboard.writeText(status.node_id); setCopied(true); } catch { setCopied(false); } }}>{copied ? '已复制 Node ID' : '复制 Node ID'}</button></section>
    <div className="section-heading"><h2>接收请求 <span className="count-tag">{requests.length}</span></h2><span>由你决定接收内容和保存位置</span></div>
    {!requests.length && <section className="surface empty-state generous"><Icon name="receive" size={38} /><h3>准备好接收了</h3><p>让另一台设备选择文件并发送给你。<br />收到请求后，会在这里等待你的确认。</p><span className="caption">默认每次询问</span></section>}
    {requests.map(task => <RequestCard key={task.id} task={task} directory={status.receive_directory} refresh={refresh} />)}
  </>;
}
