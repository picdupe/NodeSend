import { useReceiveDirectoryLabel } from '../../hooks/useReceiveDirectoryLabel';
import { useEffect, useRef, useState } from 'react';
import type { ConflictPolicy, FileEntry, NetworkStatus, TransferTask } from '../../types/network';
import { chooseReceiveDirectory, decideIncoming, errorText, listTaskEntries } from '../../api/network';
import { Icon } from '../Icon';
import { formatBytes } from './format';
/** 清单分页大小：清单可能达百万条，必须按需分页加载，不能一次传入前端。 */
const PAGE_SIZE = 200;
function RequestCard({ task, directory, refresh }: { task: TransferTask; directory: string; refresh: () => Promise<void> }) {
  const [destination, setDestination] = useState(directory);
  const manuallySelected = useRef(false);
  useEffect(() => { if (!manuallySelected.current) setDestination(directory); }, [directory]);
  const destinationLabel = useReceiveDirectoryLabel(destination);
  const [policy, setPolicy] = useState<ConflictPolicy>('rename');
  // 默认接收全部（`selected=null`）；仅当用户切到「选择部分」时才逐页挑选。
  const [all, setAll] = useState(true);
  const [selected, setSelected] = useState<string[]>([]);
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [total, setTotal] = useState(task.file_count);
  const [loading, setLoading] = useState(false);
  const [trustDevice, setTrustDevice] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  async function choose() { try { const path = await chooseReceiveDirectory(); if (path) { manuallySelected.current = true; setDestination(path); } } catch (e) { setError(errorText(e)); } }
  /** 追加加载一页清单明细（首次加载 offset=0）。 */
  async function loadMore(from: number) {
    setLoading(true);
    try {
      const page = await listTaskEntries(task.id, from, PAGE_SIZE);
      setTotal(page.total);
      setEntries(old => [...old, ...page.entries]);
    } catch (e) { setError(errorText(e)); } finally { setLoading(false); }
  }
  async function decide(accepted: boolean) { setBusy(true); try { await decideIncoming(task.id, accepted, destination, policy, all ? null : selected, accepted && trustDevice); await refresh(); } catch (e) { setError(errorText(e)); } finally { setBusy(false); } }
  const files = Math.max(0, task.file_count - task.directory_count);
  const rootsReady = task.top_level_count === 0 || task.file_count >= task.top_level_count;
  return <article className="surface request-card">
    <div className="request-heading"><span className="device-icon"><Icon name="receive" /></span><div><h3>{task.peer_name} 想发送文件</h3><p>{files} 个文件{task.directory_count > 0 ? ' · ' + task.directory_count + ' 个目录' : ''} · {task.scan_complete ? formatBytes(task.total_bytes) : '--（正在扫描）'} · 身份已验证</p></div></div>
    <p className="caption mono">Node ID：{task.peer_id}</p>
    <details onToggle={event => { if ((event.target as HTMLDetailsElement).open && !all && !entries.length && !loading) void loadMore(0); }}>
      <summary>查看并选择接收内容</summary>
      <div className="receive-options"><div className="segments" role="group" aria-label="接收范围"><button type="button" aria-pressed={all} className={all ? 'active' : ''} onClick={() => setAll(true)}>接收全部</button><button type="button" aria-pressed={!all} className={!all ? 'active' : ''} onClick={() => { setAll(false); if (!entries.length) void loadMore(0); }}>选择部分</button></div></div>
      {!all && <div className="request-files">
        {entries.map(e => <label className="check-label" key={e.path}><input type="checkbox" checked={selected.includes(e.path)} onChange={event => setSelected(old => event.target.checked ? [...old, e.path] : old.filter(p => p !== e.path))} /><span className="check-mark" aria-hidden="true"><Icon name="check" size={14} /></span><span>{e.path}</span><small>{e.directory ? '目录' : formatBytes(e.size)}</small></label>)}
        {!entries.length && !loading && <p className="caption">没有可显示的条目</p>}
        {entries.length < total && <button type="button" className="text-button" disabled={loading} onClick={() => void loadMore(entries.length)}>{loading ? '加载中…' : `加载更多（已显示 ${entries.length}/${total}）`}</button>}
      </div>}
    </details>
    <div className="receive-options"><div><label>保存到</label><button className="folder-button" onClick={choose} title={destinationLabel}><Icon name="folder" size={16} /><span>{destinationLabel}</span><span>更改</span></button></div><label>同名文件<select value={policy} onChange={e => setPolicy(e.target.value as ConflictPolicy)}><option value="rename">保留两份（重命名）</option><option value="skip">跳过已有文件</option><option value="overwrite">覆盖同路径文件</option></select></label></div>
    {policy === 'overwrite' && <p className="caption">确认接收将覆盖所选目录中的同路径文件。</p>}
    <label className="check-label"><input type="checkbox" checked={trustDevice} disabled={busy} onChange={event => setTrustDevice(event.target.checked)} /><span className="check-mark" aria-hidden="true"><Icon name="check" size={14} /></span><span>信任此设备，以后自动接收（自动拒绝策略除外）</span></label>
    {error && <div className="notice error" role="alert">{error}</div>}
    {!rootsReady && <p className="caption">正在读取所选文件和目录，收到全部顶层项目后即可确认。</p>}
    <div className="request-actions"><button className="danger" disabled={busy} onClick={() => decide(false)}>拒绝</button><button className="primary" disabled={busy || !rootsReady || (!all && !selected.length)} onClick={() => decide(true)}><Icon name="check" size={17} />确认接收</button></div>
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