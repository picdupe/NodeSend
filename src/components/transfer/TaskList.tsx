import { ReceiveDirectoryLabel } from '../../hooks/useReceiveDirectoryLabel';
import { useState } from 'react';
import { transferAction, errorText } from '../../api/network';
import type { TransferTask } from '../../types/network';
import { Icon } from '../Icon';
import { formatBytes, statusLabels, terminal } from './format';
export function TaskList({ tasks, refresh }: { tasks: TransferTask[]; refresh: () => Promise<void> }) {
  const [filter, setFilter] = useState('all'); const [search, setSearch] = useState(''); const [error, setError] = useState('');
  const [busy, setBusy] = useState<string[]>([]);
  async function action(id: string, op: string) { setBusy(old => [...old, id]); try { await transferAction(id, op); await refresh(); } catch (e) { setError(errorText(e)); } finally { setBusy(old => old.filter(v => v !== id)); } }
  const visible = tasks.filter(t => (filter === 'all' || filter === t.direction || (filter === 'active' && !terminal(t.status))) && (t.entries.map(e => e.path).join(' ') + t.peer_name).toLowerCase().includes(search.toLowerCase()));
  return <>
    <div className="task-toolbar"><div className="segments" role="group" aria-label="筛选传输">{[['all', '全部'], ['active', '进行中'], ['send', '发送'], ['receive', '接收']].map(([key, label]) => <button key={key} aria-pressed={filter === key} className={filter === key ? 'active' : ''} onClick={() => setFilter(key)}>{label}</button>)}</div><input className="search-field" aria-label="搜索传输" placeholder="搜索文件或设备" value={search} onChange={e => setSearch(e.target.value)} /></div>
    {error && <div role="alert" className="notice error">{error}</div>}
    {!visible.length && <section className="surface empty-state generous"><Icon name="tasks" size={38} /><h3>这里记录每一次传递</h3><p>发送和接收的任务都会保存在这里。<br />未完成的传输可暂停、恢复或重试。</p></section>}
    {visible.map(task => { const percent = task.status === 'completed' ? 100 : task.total_bytes ? Math.min(100, Math.max(0, task.completed_bytes / task.total_bytes * 100)) : 0; return <article className="surface task-card" key={task.id}>
      <div className="task-header"><span className={'task-icon ' + task.direction}><Icon name={task.direction === 'send' ? 'send' : 'receive'} /></span><div className="task-title"><h3>{task.entries[0]?.path ?? '传输任务'}{task.entries.length > 1 ? ' 等 ' + task.entries.length + ' 项' : ''}</h3><p>{task.direction === 'send' ? '发送给' : '来自'} {task.peer_name}</p></div><span className={'badge ' + (task.status === 'completed' ? 'success' : task.status === 'failed' ? 'failure' : '')}>{statusLabels[task.status]}</span></div>
      <progress max={100} value={Number(percent.toFixed(2))} aria-label="任务进度" />
      <div className="task-meta"><span>{formatBytes(task.completed_bytes)} / {formatBytes(task.total_bytes)} · <span className="transfer-speed">{formatBytes(task.bytes_per_second ?? 0)}/s</span> · {percent.toFixed(2)}%</span><span>{task.transport?.toUpperCase() ?? '等待连接'}{task.transport ? ' · TLS 1.3' : ''}{task.verify_hash ? ' · SHA-256' : ''}</span></div>
      {task.error && <div className="notice error">{task.error}</div>}
      {!!task.warnings?.length && <div className="notice" role="status"><strong>{task.status === 'completed' ? '传输已结束，部分文件因源文件变化未发送：' : '以下文件已变化，其余文件继续传输：'}</strong>{task.warnings.map((warning, i) => <div key={i}>{warning}</div>)}</div>}
      <div className="task-bottom"><details><summary>传输详情</summary><div className="task-details"><p className="mono">Node ID：{task.peer_id}</p>{task.destination && <p>保存位置：<ReceiveDirectoryLabel path={task.destination} /></p>}<p>{new Date(task.created_at).toLocaleString()}</p>{task.events.map((event, i) => <p key={i}>{event}</p>)}</div></details><div className="button-row">{!terminal(task.status) && <>{['paused', 'failed'].includes(task.status) ? <button disabled={busy.includes(task.id)} onClick={() => action(task.id, task.status === 'failed' ? 'retry' : 'resume')}><Icon name="play" size={15} />{task.status === 'failed' ? '重试' : '恢复'}</button> : task.status !== 'waiting' && <button disabled={busy.includes(task.id)} onClick={() => action(task.id, 'pause')}><Icon name="pause" size={15} />暂停</button>}<button disabled={busy.includes(task.id)} className="text-button danger" onClick={() => action(task.id, 'cancel')}>取消</button></>}</div></div>
    </article>; })}
  </>;
}
