import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from 'react';
import type { NetworkStatus } from '../types/network';
import { Icon, type IconName } from './Icon';
import { frontendPreview } from '../api/runtime';
import { useReceiveDecision } from '../hooks/useReceiveDecision';
export type PageKey = 'send' | 'receive' | 'transfers' | 'dashboard' | 'shares' | 'trusted';
const mainItems: { key: PageKey; label: string; icon: IconName }[] = [{ key: 'send', label: '发送', icon: 'send' }, { key: 'receive', label: '接收', icon: 'receive' }, { key: 'transfers', label: '传输记录', icon: 'tasks' }];
const extraItems: { key: PageKey; label: string; icon: IconName }[] = [{ key: 'shares', label: '共享中心', icon: 'folder' }, { key: 'trusted', label: '信任设备', icon: 'shield' }, { key: 'dashboard', label: '本机与设置', icon: 'settings' }];
/** 移动端侧栏按钮由各页面放进 .page-heading-actions，与刷新、发现按钮处于同一操作区。 */
const SidebarToggleContext = createContext<ReactNode>(null);
export function SidebarToggle() { return <>{useContext(SidebarToggleContext)}</>; }
export function AppLayout({ current, onNavigate, status, refresh, children }: { current: PageKey; onNavigate: (page: PageKey) => void; status: NetworkStatus | null; refresh: () => Promise<void>; children: ReactNode }) {
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [actionsCollapsed, setActionsCollapsed] = useState(false);
  const [dismissedRequest, setDismissedRequest] = useState<string | null>(null);
  const receiveDecision = useReceiveDecision(refresh);
  const touchStart = useRef<{ x: number; y: number } | null>(null);
  useEffect(() => {
    const onStart = (event: TouchEvent) => {
      const touch = event.touches[0];
      if (touch && touch.clientX >= 24 && touch.clientX <= 96) touchStart.current = { x: touch.clientX, y: touch.clientY };
    };
    const onMove = (event: TouchEvent) => {
      const start = touchStart.current;
      const touch = event.touches[0];
      if (!start || !touch || Math.abs(touch.clientY - start.y) > 48) return;
      if (touch.clientX - start.x > 48) { setSidebarOpen(true); touchStart.current = null; }
    };
    const onEnd = () => { touchStart.current = null; };
    window.addEventListener('touchstart', onStart, { passive: true });
    window.addEventListener('touchmove', onMove, { passive: true });
    window.addEventListener('touchend', onEnd, { passive: true });
    return () => { window.removeEventListener('touchstart', onStart); window.removeEventListener('touchmove', onMove); window.removeEventListener('touchend', onEnd); };
  }, []);
  const pendingTasks = status?.tasks.filter(t => t.direction === 'receive' && t.status === 'waiting') ?? [];
  const pending = pendingTasks.length;
  const request = pendingTasks.find(t => t.id !== dismissedRequest);
  async function answerRequest(accepted: boolean) { if (!request || !status) return; try { await receiveDecision.decide(request.id, accepted, status.receive_directory, 'rename', null, false); setDismissedRequest(request.id); } catch { /* The shared decision hook exposes the error on both views. */ } }
  const nav = (items: typeof mainItems) => items.map(item => <button key={item.key} className={'nav-item ' + (current === item.key ? 'active' : '')} onClick={() => { onNavigate(item.key); setSidebarOpen(false); }} aria-current={current === item.key ? 'page' : undefined}><Icon name={item.icon} /><span>{item.label}</span>{item.key === 'receive' && pending > 0 && <span className="nav-badge">{pending}</span>}</button>);
  const sidebarToggle = <span className="sidebar-controls">{current === 'send' && <button type="button" className="icon-button header-actions-toggle" aria-expanded={!actionsCollapsed} aria-label={actionsCollapsed ? '展开操作图标' : '收起操作图标'} onClick={() => setActionsCollapsed(value => !value)}><Icon name="chevron-right" /></button>}<button type="button" className="icon-button mobile-sidebar-toggle" aria-controls="app-sidebar" aria-expanded={sidebarOpen} aria-label={sidebarOpen ? '关闭导航' : '打开导航'} onClick={() => setSidebarOpen(value => !value)}><Icon name={sidebarOpen ? 'close' : 'menu'} /></button></span>;
  return <div className={'app-layout' + (current === 'send' && actionsCollapsed ? ' actions-collapsed' : '')}><aside id="app-sidebar" className={'sidebar ' + (sidebarOpen ? 'open' : '')}><div className="app-brand"><span className="brand-symbol"><i /><i /><i /></span><div>NodeSend<small>近在身边 · 连接彼此</small></div></div><nav aria-label="主导航" className="nav-list">{nav(mainItems)}<span className="nav-caption">更多功能</span>{nav(extraItems)}</nav><div className="sidebar-bottom"><span className="device-icon"><Icon name="device" /></span><div><strong>{status?.display_name ?? '本机设备'}</strong><small><span className={'status-dot ' + (status ? 'online' : '')} />{frontendPreview ? '界面预览 · 示例数据' : status ? '原生互传已就绪' : '连接服务中'}</small></div></div></aside>{sidebarOpen && <button className="sidebar-backdrop" aria-label="关闭侧栏" onClick={() => setSidebarOpen(false)} /> }<main className="content"><SidebarToggleContext.Provider value={sidebarToggle}>{children}</SidebarToggleContext.Provider><footer className="app-footer"><span>Node ↔ Node</span><span>{frontendPreview ? '界面预览 · 不传输真实文件' : '文件不经过云端'}</span></footer></main>{request && <div className="untrusted-receive-overlay" role="presentation"><section className="untrusted-receive-screen" role="alertdialog" aria-modal="true" aria-labelledby="global-receive-title"><div className="untrusted-device-icon"><Icon name={request.peer_device_type === 'android' || request.peer_device_type === 'ios' ? 'phone' : 'device'} size={74} /></div><div className="untrusted-receive-name">{request.peer_name}</div><p className="untrusted-receive-message">想给你发送 <strong>{request.directory_count > 0 ? '一个文件夹' : request.file_count === 1 ? '一个文件' : '多个文件'}</strong>{request.directory_count > 0 && <><br />共 {request.file_count - request.directory_count} 个文件</>}</p>{receiveDecision.error && <div className="notice error">{receiveDecision.error}</div>}<div className="untrusted-receive-spacer" /><button type="button" className="untrusted-settings-button" onClick={() => { setDismissedRequest(request.id); onNavigate('receive'); }}>设置</button><div className="untrusted-receive-actions"><button className="danger" disabled={receiveDecision.busy} onClick={() => void answerRequest(false)}>拒绝</button><button className="primary" disabled={receiveDecision.busy} onClick={() => void answerRequest(true)}>接受</button></div></section></div>}</div>;
}
