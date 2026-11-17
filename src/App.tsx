import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
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
import { useSendSelection } from './hooks/useSendSelection';
import { useUpdateNotification } from './hooks/useUpdateNotification';
import { UpdateDialog } from './components/UpdateDialog';
import { frontendPreview } from './api/runtime';
import { listShares } from './api/ipc';
import type { ShareConfig } from './types';
export default function App() {
  const { t } = useTranslation('common');
  const [page, setPage] = useState<PageKey>('send');
  const [shares, setShares] = useState<ShareConfig[]>([]);
  const sharesRevision = useRef(0);
  const [sendTaskId, setSendTaskId] = useState<string | null>(null);
  const [dismissedDiscoveryWarnings, setDismissedDiscoveryWarnings] = useState<string[]>([]);
  const [exitHint, setExitHint] = useState(false);
  const network = useNetwork();
  const sharePeerKey = JSON.stringify(network.status?.peers.map(({ node_id, display_name, online }) => [node_id, display_name, online]) ?? []);
  const shareNetwork = useMemo(() => {
    const status = network.status;
    return status ? {
      addresses: status.addresses,
      node_id: status.node_id,
      tcp_port: status.tcp_port,
      quic_port: status.quic_port,
      peers: status.peers.map(({ node_id, display_name, online }) => ({ node_id, display_name, online })),
    } : null;
  }, [network.status?.addresses.join('\0'), network.status?.node_id, network.status?.tcp_port, network.status?.quic_port, sharePeerKey]);
  const updateShares = useCallback((items: ShareConfig[]) => {
    sharesRevision.current += 1;
    setShares(current => JSON.stringify(current) === JSON.stringify(items) ? current : items);
  }, []);
  const discoveryWarnings = useRef<string[]>([]);
  discoveryWarnings.current = network.status?.discovery.warnings ?? [];
  const sendSelection = useSendSelection();
  const updateNotice = useUpdateNotification();
  useEffect(() => {
    let disposed = false;
    let retryTimer: number | undefined;
    const loadShares = async () => {
      const revision = sharesRevision.current;
      try {
        const items = await listShares();
        if (!disposed && sharesRevision.current === revision) updateShares(items);
      } catch {
        if (!disposed) retryTimer = window.setTimeout(() => void loadShares(), 3000);
      }
    };
    void loadShares();
    return () => {
      disposed = true;
      if (retryTimer !== undefined) window.clearTimeout(retryTimer);
    };
  }, [updateShares]);
  const currentPage = useRef<PageKey>('send');
  const pageHistory = useRef<PageKey[]>(['send']);
  const exitDeadline = useRef(0);
  const hintTimer = useRef<ReturnType<typeof setTimeout>>();
  const navigate = useCallback((next: PageKey) => {
    if (next !== currentPage.current) {
      setDismissedDiscoveryWarnings(previous => [...new Set([...previous, ...discoveryWarnings.current])]);
      pageHistory.current.push(next);
    }
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
        setDismissedDiscoveryWarnings(previous => [...new Set([...previous, ...discoveryWarnings.current])]);
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
      sendSelection.setPaths(old => [...new Set([...old, ...paths])]);
      navigate('send');
    };
    let dispose: (() => void) | undefined;
    void takeSharedFiles().then(receive).catch(() => {});
    void listen<string[]>('nodesend://shared-files', event => receive(event.payload))
      .then(unlisten => { dispose = unlisten; })
      .catch(() => {});
    return () => dispose?.();
  }, [navigate, sendSelection.setPaths]);
  return <>
    <AppLayout current={page} onNavigate={navigate} status={network.status} refresh={network.refresh}>
      {page === 'send' && exitHint && <div className="back-exit-hint" role="status">{t('common:exitHint')}</div>}
      {['send', 'receive', 'transfers'].includes(page) && <NetworkPage mode={page as TransferPage} {...network} dismissedDiscoveryWarnings={dismissedDiscoveryWarnings} selection={sendSelection} sendTaskId={sendTaskId} onSendTaskChange={setSendTaskId} />}
      {page === 'dashboard' && <DashboardPage network={network.status} onSaved={network.refresh} />}
      {page === 'shares' && <ShareCenterPage shares={shares} onSharesChange={updateShares} network={shareNetwork} />}
      {page === 'trusted' && <TrustedPage />}
    </AppLayout>
    {updateNotice.update && <UpdateDialog update={updateNotice.update} onClose={updateNotice.close} onSkip={updateNotice.skip} />}
  </>;
}
