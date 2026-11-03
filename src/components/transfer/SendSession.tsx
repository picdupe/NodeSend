import { useEffect } from 'react';
import type { NetworkStatus } from '../../types/network';
import { Icon } from '../Icon';
import { formatBytes, statusKeys } from './format';
import { useTranslation } from 'react-i18next';

export function SendSession({ taskId, status, onCancel, onClose }: { taskId: string; status: NetworkStatus; onCancel: (id: string) => void; onClose: () => void }) {
  const { t } = useTranslation(['common', 'transfer']);
  const task = status.tasks.find(item => item.id === taskId);
  const finished = !!task && ['completed', 'cancelled', 'rejected', 'incompatible'].includes(task.status);
  const awaitingDecision = !!task && ['queued', 'connecting', 'waiting'].includes(task.status);
  useEffect(() => {
    if (task && ['accepted', 'transferring', 'paused', 'remote_paused', 'completed'].includes(task.status)) onClose();
  }, [task?.id, task?.status, onClose]);
  function cancel() {
    if (!task || finished) { onClose(); return; }
    onCancel(taskId);
  }
  if (!task) return <section className="page send-session"><div className="surface empty-state"><p>{t('transfer:send.ended')}</p><button onClick={onClose}>{t('transfer:send.back')}</button></div></section>;
  const total = task.total_bytes > 0 ? task.total_bytes : 0;
  const percent = task.status === 'completed' ? 100 : total ? Math.min(100, task.completed_bytes / total * 100) : 0;
  return <section className="page send-session">
    <header className="page-heading"><div><span className="eyebrow">NODESEND / SEND</span><h1>{task.status === 'completed' ? t('transfer:send.complete') : task.status === 'rejected' ? t('transfer:send.peerDeclined') : task.status === 'failed' ? t('transfer:send.failed') : t('transfer:send.sending')}</h1><p>{task.status === 'waiting' ? t('transfer:send.waitingDescription') : task.status === 'transferring' ? t('transfer:send.transferringDescription') : task.status === 'completed' ? t('transfer:send.delivered') : t('transfer:send.liveStatus')}</p></div></header>
    <div className="send-session-flow surface">
      <div className="send-session-device"><span className="task-icon send"><Icon name={/Android|iPhone|iPad/i.test(navigator.userAgent) ? 'phone' : 'device'} /></span><strong>{status.display_name}</strong><small>{t('transfer:send.localDevice')}</small></div>
      <div className="send-session-arrow"><Icon name="arrow" size={28} /></div>
      <div className="send-session-device"><span className="task-icon receive"><Icon name={['android', 'ios'].includes(task.peer_device_type ?? '') ? 'phone' : 'device'} /></span><strong>{task.peer_name}</strong><small>{t('transfer:send.receivingDevice')}</small></div>
    </div>
    <div className="send-session-status surface">
      <div className="task-header"><div className="task-title"><h2>{task.file_name || t('transfer:send.selectedContent')}</h2><p>{t('transfer:send.fileCount', { count: task.file_count })}{task.directory_count ? ` · ${t('transfer:send.directoryCount', { count: task.directory_count })}` : ''}</p></div><span className="badge">{t(`transfer:${statusKeys[task.status]}`)}</span></div>
      <progress max={100} value={Number(percent.toFixed(2))} />
      <div className="task-meta"><span>{formatBytes(task.completed_bytes)} / {total ? formatBytes(total) : '--'} · {percent.toFixed(2)}%</span><span>{task.transport?.toUpperCase() ?? t('transfer:send.waitConnection')}</span></div>
      {task.error && <div className="notice error">{task.error}</div>}
      <div className="send-session-actions">{awaitingDecision ? <button onClick={cancel}><Icon name="close" size={16} />{t('common:actions.cancel')}</button> : <button onClick={onClose}><Icon name="close" size={16} />{t('common:actions.close')}</button>}</div>
    </div>
  </section>;
}
