import { ReceiveDirectoryLabel } from '../../hooks/useReceiveDirectoryLabel';
import { useTranslation } from 'react-i18next';
import { useEffect, useState } from 'react';
import { transferAction, errorText } from '../../api/network';
import type { TransferTask } from '../../types/network';
import { Icon } from '../Icon';
import { formatBytes, statusKeys, terminal } from './format';
import { formatTaskEvent, isHiddenLegacyTaskEvent } from './taskEvent';

function isHiddenTaskEvent(value: string): boolean {
  return isHiddenLegacyTaskEvent(value) || /cache relay enabled|using app storage|MediaStore.*direct write|direct write mode|no cache directory configured|writing directly to destination|receiver confirmed the save location and conflict policy|Cancellation cleanup completed|User action:|Trusted device: auto-accept|Scanning the manifest/i.test(value);
}

export function TaskList({ tasks, refresh, toolbarOnly = false, hideToolbar = false }: { tasks: TransferTask[]; refresh: () => Promise<void>; toolbarOnly?: boolean; hideToolbar?: boolean }) {
  const { t } = useTranslation(['common', 'transfer']);
  const [filter, setFilter] = useState('all'); const [search, setSearch] = useState(''); const [error, setError] = useState<{ taskId: string; message: string } | null>(null);
  const [busy, setBusy] = useState<string[]>([]);
  async function action(id: string, op: string) { setBusy(old => [...old, id]); try { await transferAction(id, op); setError(null); await refresh(); } catch (e) { setError({ taskId: id, message: errorText(e) }); } finally { setBusy(old => old.filter(v => v !== id)); } }
  useEffect(() => { if (error && tasks.some(task => task.id === error.taskId && (['accepted', 'transferring', 'completed', 'interrupted', 'remote_offline'].includes(task.status) || task.error === error.message))) setError(null); }, [tasks, error]);
  const visible = tasks.filter(t => (filter === 'all' || filter === t.direction || (filter === 'active' && !terminal(t.status))) && (t.peer_name + ' ' + t.peer_id).toLowerCase().includes(search.toLowerCase()));
  const toolbar = <div className="task-toolbar task-toolbar-fixed"><div className="segments" role="group" aria-label={t('transfer:tasks.all')}>{[['all', 'transfer:tasks.all'], ['active', 'transfer:tasks.active'], ['send', 'transfer:tasks.send'], ['receive', 'transfer:tasks.receive']].map(([key, label]) => <button key={key} aria-pressed={filter === key} className={filter === key ? 'active' : ''} onClick={() => setFilter(key)}>{t(label)}</button>)}</div><input className="search-field" aria-label={t('common:actions.search')} placeholder={t('transfer:tasks.search')} value={search} onChange={e => setSearch(e.target.value)} /></div>;
  if (toolbarOnly) return toolbar;
  return <>
    {!hideToolbar && toolbar}
    <div className="task-items-list">
    {error && !tasks.some(task => task.id === error.taskId && (task.error === error.message || ['interrupted', 'remote_offline'].includes(task.status))) && <div role="alert" className="notice error">{error.message}</div>}
    {!visible.length && <section className="surface empty-state generous"><Icon name="tasks" size={38} /><p>{t('common:empty.noTasks')}</p></section>}
    {visible.map(task => { const known = task.total_bytes > 0; const percent = known ? Math.min(100, Math.max(0, task.completed_bytes / task.total_bytes * 100)) : 0; const peerOffline = task.status === 'remote_offline'; const connectionInterrupted = task.status === 'interrupted'; const canResume = ['paused', 'remote_paused', 'offline', 'failed'].includes(task.status) || (connectionInterrupted && task.direction === 'receive'); return <article className="surface task-card" key={task.id}>
      <div className="task-header"><span className={'task-icon ' + task.direction}><Icon name={task.direction === 'send' ? 'send' : 'receive'} /></span><div className="task-title"><h3>{task.direction === 'send' ? t('transfer:tasks.send') : t('transfer:tasks.receive')} {task.peer_name}</h3><p>{t('transfer:tasks.fileCount', { count: Math.max(0, task.file_count - task.directory_count) })}{task.directory_count > 0 ? ` · ${t('transfer:tasks.directoryCount', { count: task.directory_count })}` : ''}</p></div><span className={'badge ' + (task.status === 'completed' ? 'success' : task.status === 'failed' ? 'failure' : '')}>{t(`transfer:${statusKeys[task.status]}`)}</span></div>
      <progress max={100} value={Number(percent.toFixed(2))} aria-label={t('transfer:tasks.progress')} />
      <div className="task-meta"><span>{formatBytes(task.completed_bytes)} / {known ? formatBytes(task.total_bytes) : '--'} · <span className="transfer-speed">{formatBytes(task.bytes_per_second ?? 0)}/s</span>{known ? ` · ${percent.toFixed(2)}%` : ''}{task.scan_complete ? '' : ` · ${t('transfer:tasks.scanning')}`}</span><span>{task.transport?.toUpperCase() ?? t('transfer:tasks.waitingConnection')}{task.transport ? ' · TLS 1.3' : ''}{task.verify_hash ? ' · SHA-256' : ''}</span></div>
      {task.direction === 'receive' && task.status === 'transferring' && known && task.completed_bytes >= task.total_bytes && <div className="notice" role="status">{t('transfer:tasks.finishing')}</div>}
      {!connectionInterrupted && task.error && <div className="notice error">{errorText(task.error)}</div>}
      {task.status === 'partial_completed' && <div className="notice" role="status">{t('transfer:tasks.partialNotice', { count: task.abandoned_count ?? 0 })}</div>}
      {!!task.warnings?.length && <div className="notice" role="status"><strong>{t(task.status === 'completed' ? 'transfer:tasks.completedWarnings' : 'transfer:tasks.changedWarnings')}</strong>{task.warnings.map((warning, i) => { const skippedSource = warning.match(/^Source file changed; not sent: (.*)$/); return <div key={i}>{skippedSource ? t('transfer:tasks.skippedSourceWarning', { path: skippedSource[1] }) : errorText(warning)}</div>; })}</div>}
      <div className="task-bottom"><TaskDetails task={task} /><div className="button-row">{!terminal(task.status) && <>{task.source_issue?.kind === 'missing' ? <button disabled={busy.includes(task.id)} onClick={() => action(task.id, 'finish')}>{t('transfer:tasks.complete')}</button> : task.status === 'manual_reconnecting' ? <button disabled><Icon name="play" size={15} />{t('transfer:tasks.reconnecting')}</button> : peerOffline || (connectionInterrupted && task.direction === 'send') ? <button disabled title={peerOffline ? t('transfer:tasks.offlineWait') : t('transfer:tasks.interruptedWait')}><Icon name="play" size={15} />{t('transfer:tasks.resume')}</button> : canResume ? <button disabled={busy.includes(task.id)} onClick={() => action(task.id, task.status === 'failed' ? 'retry' : 'resume')}><Icon name="play" size={15} />{task.status === 'failed' ? t('transfer:tasks.retry') : t('transfer:tasks.resume')}</button> : task.status !== 'waiting' && <button disabled={busy.includes(task.id)} onClick={() => action(task.id, 'pause')}><Icon name="pause" size={15} />{t('transfer:tasks.pause')}</button>}<button disabled={busy.includes(task.id)} className="text-button danger" onClick={() => action(task.id, 'cancel')}>{t('transfer:tasks.cancel')}</button></>}</div></div>
    </article>; })}
    </div>
  </>;
}

function TaskDetails({ task }: { task: TransferTask }) {
  const { t, i18n } = useTranslation(['common', 'transfer']);
  const [open, setOpen] = useState(false);
  return <details open={open} onToggle={event => setOpen(event.currentTarget.open)}><summary>{t('transfer:tasks.details')}</summary>{open && <div className="task-details"><p className="mono">{t('transfer:tasks.nodeId')}{task.peer_id}</p>{task.file_name && <p>{t('transfer:tasks.fileName')}{task.file_name}</p>}{task.android_media_library ? task.receive_directories.length > 0 && <p>{t('transfer:tasks.destination')}{task.receive_directories.join(' / ')}</p> : task.destination && <p>{t('transfer:tasks.destination')}<ReceiveDirectoryLabel path={task.destination} /></p>}{task.events.filter(event => !isHiddenTaskEvent(event)).map((event, i) => <p key={i}>{formatTaskEvent(event, i18n.language, t, i === 0 ? task.created_at : undefined)}</p>)}</div>}</details>;
}
