import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from 'react';
import type { NetworkStatus, TransferTask } from '../types/network';
import { Icon, type IconName } from './Icon';
import { localDeviceIcon } from '../utils/device';
import { frontendPreview } from '../api/runtime';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useReceiveDecision } from '../hooks/useReceiveDecision';
import { useTranslation } from 'react-i18next';
export type PageKey = 'send' | 'receive' | 'transfers' | 'dashboard' | 'shares' | 'trusted';
const mainItems: { key: PageKey; label: string; icon: IconName }[] = [{ key: 'send', label: 'nav.send', icon: 'send' }, { key: 'receive', label: 'nav.receive', icon: 'receive' }, { key: 'transfers', label: 'nav.transfers', icon: 'tasks' }];
const extraItems: { key: PageKey; label: string; icon: IconName }[] = [{ key: 'shares', label: 'nav.shares', icon: 'folder' }, { key: 'trusted', label: 'nav.trusted', icon: 'shield' }];
const settingsItem: { key: PageKey; label: string; icon: IconName } = { key: 'dashboard', label: 'nav.settings', icon: 'gear' };
/** 移动端侧栏按钮由各页面放进 .page-heading-actions，与刷新、发现按钮处于同一操作区。 */
const SidebarToggleContext = createContext<ReactNode>(null);
export function SidebarToggle() { return <>{useContext(SidebarToggleContext)}</>; }
export function AppLayout({ current, onNavigate, status, refresh, children }: { current: PageKey; onNavigate: (page: PageKey) => void; status: NetworkStatus | null; refresh: () => Promise<void>; children: ReactNode }) {
  const { t } = useTranslation('common');
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [dismissedRequest, setDismissedRequest] = useState<string | null>(null);
  const [visibleRequest, setVisibleRequest] = useState<TransferTask | null>(null);
  const receiveDecision = useReceiveDecision(refresh);
  const touchStart = useRef<{ x: number; y: number } | null>(null);
  useEffect(() => {
    const onStart = (event: TouchEvent) => {
      const touch = event.touches[0];
      if (!touch || event.touches.length > 1 || window.innerWidth > 850) { touchStart.current = null; return; }
      if (!sidebarOpen && event.target instanceof Element && event.target.closest('.sidebar')) return;
      touchStart.current = { x: touch.clientX, y: touch.clientY };
    };
    const onMove = (event: TouchEvent) => {
      const start = touchStart.current;
      const touch = event.touches[0];
      if (!start || !touch) return;
      const dx = touch.clientX - start.x;
      const dy = Math.abs(touch.clientY - start.y);
      if (Math.abs(dx) > 56 && Math.abs(dx) > dy * 1.4 && (sidebarOpen ? dx < 0 : dx > 0)) {
        setSidebarOpen(!sidebarOpen); touchStart.current = null;
      }
      else if (dy > 48) touchStart.current = null;
    };
    const onEnd = () => { touchStart.current = null; };
    window.addEventListener('touchstart', onStart, { passive: true });
    window.addEventListener('touchmove', onMove, { passive: true });
    window.addEventListener('touchend', onEnd, { passive: true });
    window.addEventListener('touchcancel', onEnd, { passive: true });
    return () => { window.removeEventListener('touchstart', onStart); window.removeEventListener('touchmove', onMove); window.removeEventListener('touchend', onEnd); window.removeEventListener('touchcancel', onEnd); };
  }, [sidebarOpen]);
  const pendingTasks = status?.tasks.filter(t => t.direction === 'receive' && t.status === 'waiting') ?? [];
  const pending = pendingTasks.length;
  useEffect(() => {
    setVisibleRequest(previous => {
      if (previous && previous.id !== dismissedRequest) {
        const latest = status?.tasks.find(task => task.id === previous.id);
        if (latest && ['waiting', 'cancelling', 'cancelled'].includes(latest.status)) return latest;
        if (!latest) return previous;
      }
      return status?.tasks.find(task => task.direction === 'receive' && task.status === 'waiting' && task.id !== dismissedRequest) ?? null;
    });
  }, [status, dismissedRequest]);
  const request = visibleRequest;
  const senderPeer = request && status?.peers.find(peer => peer.node_id === request.peer_id);
  const senderAddress = senderPeer?.endpoints[0]?.address ?? '';
  const senderIp = senderAddress.includes('.') ? senderAddress.split('.').pop() ?? '' : senderAddress.replace(/^.*:/, '').replace(/\]$/, '');
  const vendor = request ? detectVendor(request.peer_name, request.peer_device_type) : '';
  const cancelledByPeer = request?.status === 'cancelled' || request?.status === 'cancelling';
  async function answerRequest(accepted: boolean) {
    if (!request || !status || cancelledByPeer || receiveDecision.busy) return;
    if (!accepted) setDismissedRequest(request.id);
    try {
      await receiveDecision.decide(request.id, accepted, status.receive_directory, 'rename', null, false);
      if (accepted) setDismissedRequest(request.id);
    } catch {
      if (!accepted) setDismissedRequest(null);
    }
  }
  const nav = (items: typeof mainItems) => items.map(item => <button key={item.key} className={'nav-item ' + (current === item.key ? 'active' : '')} onClick={() => { onNavigate(item.key); setSidebarOpen(false); }} aria-current={current === item.key ? 'page' : undefined}><Icon name={item.icon} /><span>{t(item.label)}</span>{item.key === 'receive' && pending > 0 && <span className="nav-badge">{pending}</span>}</button>);
  const sidebarToggle = <span className="sidebar-controls"><button type="button" className="icon-button mobile-sidebar-toggle" aria-controls="app-sidebar" aria-expanded={sidebarOpen} aria-label={sidebarOpen ? t('shell.closeNav') : t('shell.openNav')} onClick={() => setSidebarOpen(value => !value)}><Icon name={sidebarOpen ? 'close' : 'menu'} /></button></span>;
  return <div className={'app-layout' + (sidebarCollapsed ? ' sidebar-collapsed' : '')}><header className="custom-titlebar" data-tauri-drag-region /><div className="app-shell"><aside id="app-sidebar" className={'sidebar ' + (sidebarOpen ? 'open' : '')}><div className="app-brand"><button type="button" className="brand-symbol rail-toggle" aria-controls="app-sidebar" aria-expanded={!sidebarCollapsed} aria-label={sidebarCollapsed ? t('shell.expandNav') : t('shell.collapseNav')} onClick={() => setSidebarCollapsed(value => !value)}><i /><i /><i /><Icon name="panel-left" /></button><span className="brand-symbol brand-mark" aria-hidden="true"><i /><i /><i /></span><div>NodeSend</div></div><nav aria-label={t('nav.main')} className="nav-list">{nav(mainItems)}{nav(extraItems)}</nav><div className="sidebar-bottom">{nav([settingsItem])}<div className="sidebar-device"><span className="device-icon"><Icon name={localDeviceIcon()} /></span><div><strong>{status?.display_name ?? t('device.local')}</strong></div></div></div></aside>{sidebarOpen && <button className="sidebar-backdrop" aria-label={t('shell.closeNav')} onClick={() => setSidebarOpen(false)} /> }<main className="content"><SidebarToggleContext.Provider value={sidebarToggle}><div className="page-scroll-region">{children}</div></SidebarToggleContext.Provider></main>{request && <div className="untrusted-receive-overlay" role="presentation"><section className="untrusted-receive-screen" role="alertdialog" aria-modal="true" aria-labelledby="global-receive-title"><div className="untrusted-receive-body"><div className="untrusted-device-icon"><Icon name={request.peer_device_type === 'android' || request.peer_device_type === 'ios' ? 'phone' : 'device'} size={74} /></div><div className="untrusted-receive-name">{request.peer_name}</div><div className="untrusted-receive-device-meta">{senderIp && <span>#{senderIp}</span>}{vendor && <span>{vendor}</span>}</div><p className="untrusted-receive-message">{t('shell.requestFiles')} <strong>{request.directory_count > 0 ? t('shell.oneFolder') : request.file_count === 1 ? t('shell.oneFile') : t('shell.manyFiles')}</strong>{request.directory_count > 0 && <><br />{t('shell.fileCount', { count: request.file_count - request.directory_count })}</>}</p></div>
        <div className="untrusted-receive-footer">
          {receiveDecision.error && <div className="notice error">{receiveDecision.error}</div>}
          {cancelledByPeer ? <>
            <p className="untrusted-cancel-message" role="status">{t('shell.peerCancelled')}</p>
            <div className="untrusted-receive-actions cancelled"><button type="button" className="primary" onClick={() => setDismissedRequest(request.id)}><Icon name="close" size={17} />{t('actions.close')}</button></div>
          </> : <>
            <button type="button" className="untrusted-settings-button" onClick={() => { setDismissedRequest(request.id); onNavigate('receive'); }}><Icon name="settings" size={17} />{t('shell.settings')}</button>
            <div className="untrusted-receive-actions"><button type="button" className="danger" disabled={receiveDecision.busy} onClick={() => void answerRequest(false)}><Icon name="close" size={17} />{t('actions.decline')}</button><button type="button" className="primary" disabled={receiveDecision.busy} onClick={() => void answerRequest(true)}><Icon name="check" size={17} />{t('actions.accept')}</button></div>
          </>}
        </div></section></div>}</div></div>;
}

/** 桌面窗口按钮：所有页面统一放在页头图标按钮区的最右侧。 */
export function WindowControls() {
  if (frontendPreview) return null;
  const { t } = useTranslation('common');
  return <>
    <button type="button" className="icon-button window-control-button" aria-label={t('shell.minimize')} onClick={() => void getCurrentWindow().minimize()}><Icon name="window-minimize" /></button>
    <button type="button" className="icon-button window-control-button" aria-label={t('shell.maximize')} onClick={() => void getCurrentWindow().toggleMaximize()}><Icon name="window-maximize" /></button>
    <button type="button" className="icon-button window-control-button window-close" aria-label={t('actions.close')} onClick={() => void getCurrentWindow().close()}><Icon name="close" /></button>
  </>;
}

function detectVendor(name: string, deviceType?: string): string {
  const value = `${name} ${deviceType ?? ''}`.toLowerCase();
  const brands: Array<[RegExp, string]> = [
    [/redmi/, 'redmi'], [/xiaomi|miui|mi\s?\d/, 'xiaomi'], [/vivo|iqoo/, 'vivo'],
    [/oppo|oneplus|realme/, 'oppo'], [/huawei|mate|honor/, 'huawei'],
    [/samsung|galaxy/, 'samsung'], [/google|pixel/, 'google'], [/apple|iphone|ipad/, 'apple'],
    [/motorola|moto/, 'motorola'], [/sony|xperia/, 'sony'], [/asus|rog\s?phone/, 'asus'],
    [/lenovo|legion/, 'lenovo'], [/nokia/, 'nokia'], [/meizu/, 'meizu'],
    [/zte|nubia|red\s?magic/, 'zte'], [/nothing/, 'nothing'], [/microsoft|surface/, 'microsoft'],
  ];
  return brands.find(([pattern]) => pattern.test(value))?.[1] ?? '';
}
