import { useReceiveDirectoryLabel } from '../hooks/useReceiveDirectoryLabel';
import { useEffect, useRef, useState } from 'react';
import { getNodeInfo } from '../api/ipc';
import { frontendPreview } from '../api/runtime';
import { getVersion } from '@tauri-apps/api/app';
import { chooseReceiveDirectory, requestMediaLibraryPermission, setAndroidMediaLibraryEnabled, setNetworkSettings, errorText } from '../api/network';
import { checkUpdate, checkAndroidUpdate } from '../api/update';
import { resolveUpdateEndpoints } from '../services/updateSources';
import { getStoredUpdateChannel, setStoredUpdateChannel, openUpdatePrompt } from '../services/updatePrompt';
import type { NodeInfo } from '../types';
import type { NetworkStatus } from '../types/network';
import { Icon } from '../components/Icon';
import { SidebarToggle, WindowControls } from '../components/AppLayout';
import { MediaLibrarySetting } from '../components/MediaLibrarySetting';

import { addressPriority } from '../utils/address';
import { displayVersion, displayBuildVersion } from '../utils/version';

export function DashboardPage({ network, onSaved }: { network: NetworkStatus | null; onSaved: () => Promise<void> }) {
  const addresses = [...(network?.addresses ?? [])].sort((a, b) => addressPriority(a) - addressPriority(b));
  const [info, setInfo] = useState<NodeInfo | null>(null); const [error, setError] = useState(''); const [busy, setBusy] = useState(false);
  const [toasts, setToasts] = useState<{ id: number; text: string }[]>([]);
  const toastId = useRef(0);
  const [mediaLibraryBusy, setMediaLibraryBusy] = useState(false);
  const [appVersion, setAppVersion] = useState('');
  const [name, setName] = useState(network?.display_name ?? ''); const [directory, setDirectory] = useState(network?.receive_directory ?? ''); const [cacheDirectory, setCacheDirectory] = useState(network?.cache_directory ?? ''); const [policy, setPolicy] = useState(network?.receive_policy ?? 'ask');
  const [saveToGallery, setSaveToGallery] = useState(network?.media_library_enabled ?? false);
  const directoryLabel = useReceiveDirectoryLabel(directory);
  const android = /Android/i.test(navigator.userAgent);
  type SettingsValues = { name: string; directory: string; cacheDirectory: string; policy: string };
  const saved = useRef<SettingsValues>({ name: network?.display_name ?? '', directory: network?.receive_directory ?? '', cacheDirectory: network?.cache_directory ?? '', policy: network?.receive_policy ?? 'ask' });
  const latest = useRef<SettingsValues>({ name: network?.display_name ?? '', directory: network?.receive_directory ?? '', cacheDirectory: network?.cache_directory ?? '', policy: network?.receive_policy ?? 'ask' });
  latest.current = { name, directory, cacheDirectory, policy };
  const saveQueue = useRef<Promise<void>>(Promise.resolve());
  const mounted = useRef(true);
  const initialized = useRef(network !== null);
  const [policyOpen, setPolicyOpen] = useState(false); const [policyFlip, setPolicyFlip] = useState(false);
  const [updateChannel, setUpdateChannel] = useState<'stable' | 'test'>(getStoredUpdateChannel);
  const [updateChannelOpen, setUpdateChannelOpen] = useState(false); const [updateChannelFlip, setUpdateChannelFlip] = useState(false);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [updateMessage, setUpdateMessage] = useState('');
  const [updateError, setUpdateError] = useState('');
  const policyOptions = [{ value: 'ask', label: '每次询问', detail: '收到文件前确认一次' }, { value: 'trusted', label: '仅自动接收已信任设备', detail: '信任设备可直接保存' }, { value: 'reject', label: '自动拒绝', detail: '所有接收请求都拒绝' }] as const;
  const selectedPolicy = policyOptions.find(option => option.value === policy) ?? policyOptions[0];
  useEffect(() => {
    getNodeInfo().then(setInfo).catch(e => setError(errorText(e)));
    getVersion().then(setAppVersion).catch(() => setAppVersion(''));
  }, []);
  useEffect(() => {
    if (!network || initialized.current) return;
    setName(network.display_name); setDirectory(network.receive_directory); setCacheDirectory(network.cache_directory); setPolicy(network.receive_policy); setSaveToGallery(network.media_library_enabled);
    saved.current = { name: network.display_name, directory: network.receive_directory, cacheDirectory: network.cache_directory, policy: network.receive_policy };
    initialized.current = true;
  }, [network]);
  // 离开设置页时兜底提交尚未失焦的修改（失焦已提交的部分会因无变化而跳过）。
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; persist(); };
  }, []);
  /** 浮层提示：固定在卡片标题行右侧空白带内水平居中，只淡入淡出；新提示直接替换旧提示，不堆叠、不下移。 */
  function showToast(text: string) {
    const id = ++toastId.current;
    setToasts([{ id, text }]);
    window.setTimeout(() => setToasts(list => list.filter(item => item.id !== id)), 2600);
  }
  /** 自动保存：仅在内容真正变化时写入；连续触发按顺序串行执行，后一次快照覆盖前一次。 */
  function persist(overrides: Partial<SettingsValues> = {}) {
    const next = { ...latest.current, ...overrides };
    latest.current = next;
    saveQueue.current = saveQueue.current.then(async () => {
      const before = saved.current;
      if (next.name === before.name && next.directory === before.directory && next.cacheDirectory === before.cacheDirectory && next.policy === before.policy) return;
      const changedName = next.name !== before.name;
      if (mounted.current) { setBusy(true); setError(''); }
      try {
        await setNetworkSettings(next.name, next.directory, next.cacheDirectory, next.policy);
        saved.current = next;
        await onSaved();
        if (mounted.current) showToast(changedName ? '设备名称已保存，发现广播中的名称将在下次启动时更新。' : '已保存。');
      } catch (e) {
        if (mounted.current) setError(errorText(e));
      } finally {
        if (mounted.current) setBusy(false);
      }
    });
  }
  async function toggleSaveToGallery(enabled: boolean) {
    setMediaLibraryBusy(true); setError('');
    try {
      if (enabled) {
        const granted = await requestMediaLibraryPermission();
        if (!granted) { setError('未授予相册权限，文件仍保存到当前接收目录。'); return; }
      }
      await setAndroidMediaLibraryEnabled(enabled);
      setSaveToGallery(enabled);
      await onSaved();
      showToast(enabled ? '默认媒体目录已启用。' : '默认媒体目录已关闭，文件将保存到当前接收目录。');
    } catch (e) { setError(errorText(e)); }
    finally { setMediaLibraryBusy(false); }
  }
  async function checkUpdates() {
    setCheckingUpdate(true); setUpdateMessage(''); setUpdateError('');
    try {
      // In browser preview, show the dialog with a simulated update so its content can be reviewed.
      if (frontendPreview) {
        openUpdatePrompt({
          version: '0.1.4-build.126',
          currentVersion: '0.1.4-build.125',
          notes: '界面预览：此处内容来自更新清单 latest.json 的 notes 字段。\n· 修复已知问题\n· 提升传输稳定性',
          publishedAt: null,
          downloadUrl: '#preview-update',
          releasePageUrl: '#preview-update',
          channel: updateChannel,
        });
        setUpdateMessage('预览模式：已在弹窗中展示模拟更新。');
        return;
      }
      if (android) {
        const update = await checkAndroidUpdate(updateChannel);
        if (!update) { setUpdateMessage('当前 Android 版本已是最新版本。'); return; }
        openUpdatePrompt(update);
        setUpdateMessage(`发现新版本 ${displayVersion(update.version)}，已在更新弹窗中提示。`);
        return;
      }
      const endpoints = await resolveUpdateEndpoints(updateChannel);
      if (!endpoints.sources.length) {
        throw new Error('当前更新通道没有配置更新源，请编辑 AppData\\Roaming\\NodeSend\\update-sources.json。');
      }
      const update = await checkUpdate(updateChannel, endpoints);
      if (!update) {
        setUpdateMessage(`当前版本 ${displayVersion(await getVersion())}，已是最新版本。`);
        return;
      }
      openUpdatePrompt(update);
      setUpdateMessage(`发现新版本 ${displayVersion(update.version)}，已在更新弹窗中提示。`);
    } catch (e) {
      setUpdateError(e instanceof Error ? e.message : errorText(e));
    } finally { setCheckingUpdate(false); }
  }
  return <div className="page settings-page"><header className="page-heading"><div><span className="eyebrow">NODESEND / SETTINGS</span></div><div className="page-heading-actions"><SidebarToggle /><WindowControls /></div></header>
    {error && <div className="notice error" role="alert">{error}</div>}
    <section className="surface settings-card">{toasts.length > 0 && <div className="save-toast-layer" role="status" aria-live="polite">{toasts.map(item => <div className="save-toast" key={item.id}>{item.text}</div>)}</div>}<h2>本机设备</h2><div className="form-stack"><label>设备名称<input value={name} maxLength={64} onChange={e => setName(e.target.value)} onBlur={() => persist()} /></label><label>默认接收目录<div className="button-row"><input value={android ? directoryLabel : directory} readOnly={android} onChange={e => setDirectory(e.target.value)} onBlur={android ? undefined : () => persist()} /><button type="button" onClick={async () => { try { const path = await chooseReceiveDirectory(); if (path) { setDirectory(path); persist({ directory: path }); } } catch (e) { setError(errorText(e)); } }}>选择</button></div></label>{android && <MediaLibrarySetting enabled={saveToGallery} busy={busy || mediaLibraryBusy} onChange={enabled => void toggleSaveToGallery(enabled)} />}{!android && <label>默认缓存目录<div className="button-row"><input value={cacheDirectory} onChange={e => setCacheDirectory(e.target.value)} onBlur={() => persist()} placeholder="不使用缓存" /><button type="button" onClick={async () => { try { const path = await chooseReceiveDirectory(); if (path) { setCacheDirectory(path); persist({ cacheDirectory: path }); } } catch (e) { setError(errorText(e)); } }}>选择</button><button type="button" disabled={!cacheDirectory} onClick={() => { setCacheDirectory(''); persist({ cacheDirectory: '' }); }}>不使用缓存</button></div></label>}<label className="policy-field">接收策略<div className="policy-picker"><button type="button" className="policy-trigger" aria-haspopup="listbox" aria-expanded={policyOpen} onClick={event => { const rect = event.currentTarget.getBoundingClientRect(); setPolicyFlip(window.innerHeight - rect.bottom < 180); setPolicyOpen(value => !value); }}><span><strong>{selectedPolicy.label}</strong><small>{selectedPolicy.detail}</small></span><Icon name={policyOpen ? 'chevron-up' : 'chevron-down'} size={18} /></button>{policyOpen && <div className={'policy-menu ' + (policyFlip ? 'flip' : '')} role="listbox" aria-label="接收策略选项">{policyOptions.map(option => <button type="button" role="option" aria-selected={option.value === policy} className={'policy-option ' + (option.value === policy ? 'selected' : '')} key={option.value} onClick={() => { setPolicy(option.value); setPolicyOpen(false); persist({ policy: option.value }); }}><span><strong>{option.label}</strong><small>{option.detail}</small></span>{option.value === policy && <Icon name="check" size={17} />}</button>)}</div>}</div></label><div className="update-check-field"><label>检查更新</label><div className="update-check-row"><div className="policy-picker update-channel-picker"><button type="button" className="policy-trigger" aria-haspopup="listbox" aria-expanded={updateChannelOpen} onClick={event => { const rect = event.currentTarget.getBoundingClientRect(); setUpdateChannelFlip(window.innerHeight - rect.bottom < 130); setUpdateChannelOpen(value => !value); }}><span><strong>{updateChannel === 'stable' ? '稳定版' : '测试版'}</strong></span><Icon name={updateChannelOpen ? 'chevron-up' : 'chevron-down'} size={18} /></button>{updateChannelOpen && <div className={'policy-menu ' + (updateChannelFlip ? 'flip' : '')} role="listbox" aria-label="更新通道"><button type="button" role="option" aria-selected={updateChannel === 'stable'} className={'policy-option ' + (updateChannel === 'stable' ? 'selected' : '')} onClick={() => { setUpdateChannel('stable'); setStoredUpdateChannel('stable'); setUpdateChannelOpen(false); setUpdateMessage(''); setUpdateError(''); }}><span><strong>稳定版</strong></span>{updateChannel === 'stable' && <Icon name="check" size={17} />}</button><button type="button" role="option" aria-selected={updateChannel === 'test'} className={'policy-option ' + (updateChannel === 'test' ? 'selected' : '')} onClick={() => { setUpdateChannel('test'); setStoredUpdateChannel('test'); setUpdateChannelOpen(false); setUpdateMessage(''); setUpdateError(''); }}><span><strong>测试版</strong></span>{updateChannel === 'test' && <Icon name="check" size={17} />}</button></div>}</div><button type="button" className="primary" disabled={checkingUpdate} onClick={() => void checkUpdates()}>{checkingUpdate ? '检查中…' : '检查更新'}</button></div>{updateMessage && <p className="hint ok" role="status">{updateMessage}</p>}{updateError && <p className="hint error" role="alert">{updateError}</p>}</div></div><section className="settings-network-details" aria-labelledby="settings-network-title"><h3 id="settings-network-title">身份与网络详情</h3><dl className="detail-list"><dt>Node ID</dt><dd className="mono">{network?.node_id ?? info?.node_id}</dd><dt>QUIC / TCP</dt><dd>{network?.quic_port} / {network?.tcp_port} · TLS 1.3</dd><dt>本机地址</dt><dd>{addresses.join(' · ')}</dd><dt>自动发现</dt><dd>mDNS {network?.discovery.mdns ? '运行中' : '不可用'} · UDP {network?.discovery.udp ? '运行中' : '不可用'}</dd><dt>版本号</dt><dd>{appVersion ? displayBuildVersion(appVersion) : '读取中…'}</dd></dl></section></section>
  </div>;
}
