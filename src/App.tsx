import { useCallback, useEffect, useRef, useState } from 'react';
import { getCurrent, onOpenUrl } from '@tauri-apps/plugin-deep-link';
import { listen } from '@tauri-apps/api/event';
import { exit, onBackButtonPress } from '@tauri-apps/api/app';
import { takeSharedFiles } from './api/network';
import { AppLayout, type PageKey } from './components/AppLayout';
import { DashboardPage } from './pages/DashboardPage';
import { ShareCenterPage } from './pages/ShareCenterPage';
import { TrustedPage } from './pages/TrustedPage';
import { NetworkPage, type TransferPage } from './pages/NetworkPage';
import { useNetwork } from './hooks/useNetwork';
import { frontendPreview } from './api/runtime';
export default function App() {
  const [page, setPage] = useState<PageKey>('send');
  const [exitHint, setExitHint] = useState(false);
  const network = useNetwork();
  const currentPage = useRef<PageKey>('send');
  const pageHistory = useRef<PageKey[]>(['send']);
  const exitDeadline = useRef(0);
  const hintTimer = useRef<ReturnType<typeof setTimeout>>();
  const navigate = useCallback((next: PageKey) => {
    if (next !== currentPage.current) pageHistory.current.push(next);
    currentPage.current = next;
    exitDeadline.current = 0;
    clearTimeout(hintTimer.current);
    setExitHint(false);
    setPage(next);
    if (next === 'transfers') requestAnimationFrame(() => document.querySelector<HTMLElement>('.content')?.scrollTo({ top: 0 }));
  }, []);
  useEffect(() => {
    const openTransfers = () => navigate('transfers');
    window.addEventListener('nodesend-open-transfers', openTransfers);
    document.documentElement.dataset.nodeSendNavigationReady = 'true';
    return () => { window.removeEventListener('nodesend-open-transfers', openTransfers); delete document.documentElement.dataset.nodeSendNavigationReady; };
  }, [navigate]);
  useEffect(() => {
    if (frontendPreview) return;
    let disposed = false;
    let unregister: (() => void) | undefined;
    void onBackButtonPress(() => {
      if (currentPage.current !== 'send') {
        pageHistory.current.pop();
        const previous = pageHistory.current[pageHistory.current.length - 1] ?? 'send';
        currentPage.current = previous;
        exitDeadline.current = 0;
        clearTimeout(hintTimer.current);
        setExitHint(false);
        setPage(previous);
        if (previous === 'transfers') requestAnimationFrame(() => document.querySelector<HTMLElement>('.content')?.scrollTo({ top: 0 }));
        return;
      }
      if (Date.now() < exitDeadline.current) { void exit(0); return; }
      exitDeadline.current = Date.now() + 2000;
      setExitHint(true);
      clearTimeout(hintTimer.current);
      hintTimer.current = setTimeout(() => setExitHint(false), 2000);
    }).then(listener => {
      if (disposed) { void listener.unregister(); return; }
      unregister = () => { void listener.unregister(); };
    }).catch(() => {});
    return () => { disposed = true; unregister?.(); clearTimeout(hintTimer.current); };
  }, []);
  useEffect(() => {
    if (frontendPreview) return;
    function consume(urls: string[]) {
      for (const raw of urls) {
        try {
          const url = new URL(raw);
          if (url.protocol !== 'ns:') continue;
          if (url.hostname === 'node') {
            const host = url.searchParams.get('host');
            if (!host) continue;
            const tcp = url.searchParams.get('tcp');
            const address = tcp ? (host.includes(':') && !host.startsWith('[') ? `[${host}]:${tcp}` : `${host}:${tcp}`) : host;
            window.dispatchEvent(new CustomEvent('nodesend-deep-link', { detail: { kind: 'node', address, nodeId: url.searchParams.get('node') } }));
            navigate('send');
          } else if (url.hostname === 'share') {
            const httpUrl = url.searchParams.get('http');
            if (!httpUrl) continue;
            window.dispatchEvent(new CustomEvent('nodesend-deep-link', { detail: { kind: 'share', httpUrl } }));
            navigate('shares');
          }
        } catch { /* Ignore unrelated command-line arguments. */ }
      }
    }
    let dispose: (() => void) | undefined;
    void getCurrent().then(urls => { if (urls) consume(urls); }).catch(() => {});
    void onOpenUrl(urls => consume(urls)).then(unlisten => { dispose = unlisten; }).catch(() => {});
    return () => dispose?.();
  }, []);
  useEffect(() => {
    if (frontendPreview) return;
    const receive = (paths: string[]) => {
      if (!paths.length) return;
      window.dispatchEvent(new CustomEvent('nodesend-shared-files', { detail: paths }));
      navigate('send');
    };
    let dispose: (() => void) | undefined;
    void takeSharedFiles().then(receive).catch(() => {});
    void listen<string[]>('nodesend://shared-files', event => receive(event.payload))
      .then(unlisten => { dispose = unlisten; })
      .catch(() => {});
    return () => dispose?.();
  }, []);
  return <AppLayout current={page} onNavigate={navigate} status={network.status}>
    {page === 'send' && exitHint && <div className="back-exit-hint" role="status">再按一次返回退出</div>}
    {['send', 'receive', 'transfers'].includes(page) && <NetworkPage mode={page as TransferPage} {...network} onNavigate={navigate} />}
    {page === 'dashboard' && <DashboardPage network={network.status} onSaved={network.refresh} />}
    {page === 'shares' && <ShareCenterPage />}
    {page === 'trusted' && <TrustedPage />}
  </AppLayout>;
}
