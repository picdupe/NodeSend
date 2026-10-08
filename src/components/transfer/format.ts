import type { TaskStatus } from '../../types/network';
export const formatBytes = (n: number): string => { if (n < 1024) return n.toFixed(2) + ' B'; const unit = Math.min(4, Math.floor(Math.log(n) / Math.log(1024))); return (n / 1024 ** unit).toFixed(2) + ' ' + ['B', 'KB', 'MB', 'GB', 'TB'][unit]; };
export const statusLabels: Record<TaskStatus, string> = { queued: '排队中', connecting: '正在连接', waiting: '等待接收确认', accepted: '已接受', transferring: '传输中', paused: '已暂停', interrupted: '正在重连', failed: '需要处理', completed: '已完成', rejected: '已拒绝', cancelled: '已取消' };
export const terminal = (status: TaskStatus) => ['completed', 'rejected', 'cancelled'].includes(status);

