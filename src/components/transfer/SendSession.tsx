import { useEffect } from 'react';
import type { NetworkStatus } from '../../types/network';
import { Icon } from '../Icon';
import { formatBytes, statusLabels } from './format';

export function SendSession({ taskId, status, onCancel, onClose }: { taskId: string; status: NetworkStatus; onCancel: (id: string) => void; onClose: () => void }) {
  const task = status.tasks.find(item => item.id === taskId);
  const finished = !!task && ['completed', 'cancelled', 'rejected', 'incompatible'].includes(task.status);
  const awaitingDecision = !!task && ['queued', 'connecting', 'waiting'].includes(task.status);
  useEffect(() => {
    // Once the receiver accepts, the send page is free for a new task. The
    // task continues in the transfer history while the sender view closes.
    if (task && ['accepted', 'transferring', 'paused', 'remote_paused', 'completed'].includes(task.status)) onClose();
  }, [task?.id, task?.status, onClose]);
  function cancel() {
    if (!task || finished) { onClose(); return; }
    onCancel(taskId);
  }
  if (!task) return <section className="page send-session"><div className="surface empty-state"><p>发送任务已结束。</p><button onClick={onClose}>返回发送</button></div></section>;
  const total = task.total_bytes > 0 ? task.total_bytes : 0;
  const percent = task.status === 'completed' ? 100 : total ? Math.min(100, task.completed_bytes / total * 100) : 0;
  return <section className="page send-session">
    <header className="page-heading"><div><span className="eyebrow">NODESEND / SEND</span><h1>{task.status === 'completed' ? '发送完成' : task.status === 'rejected' ? '对方已拒绝' : task.status === 'failed' ? '发送失败' : '正在发送'}</h1><p>{task.status === 'waiting' ? '等待对方确认后开始传输' : task.status === 'transferring' ? '文件正在传输中' : task.status === 'completed' ? '文件已送达对方设备' : '任务状态会在当前页面实时更新'}</p></div></header>
    <div className="send-session-flow surface">
      <div className="send-session-device"><span className="task-icon send"><Icon name={/Android|iPhone|iPad/i.test(navigator.userAgent) ? 'phone' : 'device'} /></span><strong>{status.display_name}</strong><small>本机</small></div>
      <div className="send-session-arrow"><Icon name="arrow" size={28} /></div>
      <div className="send-session-device"><span className="task-icon receive"><Icon name={['android', 'ios'].includes(task.peer_device_type ?? '') ? 'phone' : 'device'} /></span><strong>{task.peer_name}</strong><small>接收设备</small></div>
    </div>
    <div className="send-session-status surface">
      <div className="task-header"><div className="task-title"><h2>{task.file_name || '已选择的内容'}</h2><p>{task.file_count} 个文件{task.directory_count ? ` · ${task.directory_count} 个目录` : ''}</p></div><span className="badge">{statusLabels[task.status]}</span></div>
      <progress max={100} value={Number(percent.toFixed(2))} />
      <div className="task-meta"><span>{formatBytes(task.completed_bytes)} / {total ? formatBytes(total) : '--'} · {percent.toFixed(2)}%</span><span>{task.transport?.toUpperCase() ?? '等待连接'}</span></div>
      {task.error && <div className="notice error">{task.error}</div>}
      <div className="send-session-actions">{awaitingDecision ? <button onClick={cancel}><Icon name="close" size={16} />取消</button> : <button onClick={onClose}><Icon name="close" size={16} />关闭</button>}</div>
    </div>
  </section>;
}
