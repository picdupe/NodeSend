import { useReceiveDirectoryLabel } from '../hooks/useReceiveDirectoryLabel';
import { useEffect, useRef, useState } from 'react';
import { getNodeInfo } from '../api/ipc';
import { getVersion } from '@tauri-apps/api/app';
import { chooseReceiveDirectory, setNetworkSettings, errorText } from '../api/network';
import { checkUpdate, downloadUpdate as downloadUpdatePackageApi, installUpdate as installUpdateApi, openUpdateUrl } from '../api/update';
import type { UpdateDownloadProgress, UpdateInfo } from '../api/update';
import { resolveUpdateEndpoints } from '../services/updateSources';
import type { NodeInfo } from '../types';
import type { NetworkStatus } from '../types/network';
import { Icon } from '../components/Icon';
import { SidebarToggle } from '../components/AppLayout';

import { addressPriority } from '../utils/address';

function displayVersion(version: string): string {
  const normalized = version.replace(/^v/i, '').replace(/-build\.(\d+)$/i, '.$1');
  return `v${normalized}`;
}

function displayBuildVersion(version: string): string {
  const match = version.replace(/^v/i, '').match(/^(.+?)(?:-build\.(\d+)|\.(\d+))$/i);
  if (!match) return displayVersion(version);
  return `v${match[1]} Build ${match[2] ?? match[3]}`;
}

export function DashboardPage({ network, onSaved }: { network: NetworkStatus | null; onSaved: () => Promise<void> }) {
  const addresses = [...(network?.addresses ?? [])].sort((a, b) => addressPriority(a) - addressPriority(b));
  const [info, setInfo] = useState<NodeInfo | null>(null); const [error, setError] = useState(''); const [message, setMessage] = useState(''); const [busy, setBusy] = useState(false);
  const [appVersion, setAppVersion] = useState('');
  const [name, setName] = useState(network?.display_name ?? ''); const [directory, setDirectory] = useState(network?.receive_directory ?? ''); const [policy, setPolicy] = useState(network?.receive_policy ?? 'ask');
  const directoryLabel = useReceiveDirectoryLabel(directory);
  const android = /Android/i.test(navigator.userAgent);
  const saved = useRef({ name: network?.display_name ?? '', directory: network?.receive_directory ?? '', policy: network?.receive_policy ?? 'ask' });
  const initialized = useRef(network !== null);
  const [policyOpen, setPolicyOpen] = useState(false); const [policyFlip, setPolicyFlip] = useState(false);
  const [updateChannel, setUpdateChannel] = useState<'stable' | 'test'>('stable');
  const [updateChannelOpen, setUpdateChannelOpen] = useState(false); const [updateChannelFlip, setUpdateChannelFlip] = useState(false);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [downloadingUpdate, setDownloadingUpdate] = useState(false);
  const [installingUpdate, setInstallingUpdate] = useState(false);
  const [downloadedUpdate, setDownloadedUpdate] = useState(false);
  const [downloadProgress, setDownloadProgress] = useState<UpdateDownloadProgress | null>(null);
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updateMessage, setUpdateMessage] = useState('');
  const [updateError, setUpdateError] = useState('');
  const policyOptions = [{ value: 'ask', label: '每次询问', detail: '收到文件前确认一次' }, { value: 'trusted', label: '仅自动接收已信任设备', detail: '信任设备可直接保存' }, { value: 'reject', label: '自动拒绝', detail: '所有接收请求都拒绝' }] as const;
  const selectedPolicy = policyOptions.find(option => option.value === policy) ?? policyOptions[0];
  const hasActiveTransfers = network?.tasks.some(task => ['queued', 'connecting', 'waiting', 'accepted', 'transferring', 'interrupted'].includes(task.status)) ?? false;
  useEffect(() => {
    getNodeInfo().then(setInfo).catch(e => setError(errorText(e)));
    getVersion().then(setAppVersion).catch(() => setAppVersion(''));
  }, []);
  useEffect(() => {
    if (!network || initialized.current) return;
    setName(network.display_name); setDirectory(network.receive_directory); setPolicy(network.receive_policy);
    saved.current = { name: network.display_name, directory: network.receive_directory, policy: network.receive_policy };
    initialized.current = true;
  }, [network]);
  async function save() {
    const changedName = name !== saved.current.name;
    const changedDirectory = directory !== saved.current.directory;
    const changedPolicy = policy !== saved.current.policy;
    if (!changedName && !changedDirectory && !changedPolicy) { setMessage('设置未发生变化。'); return; }
    setBusy(true); setError('');
    try {
      await setNetworkSettings(name, directory, policy); await onSaved();
      saved.current = { name, directory, policy };
      const changes = [changedDirectory && '默认接收目录', changedPolicy && '接收策略'].filter(Boolean).join('和');
      setMessage(changedName ? `设备名称已保存，发现广播中的名称将在下次启动时更新。${changes ? `已同时保存${changes}。` : ''}` : `已保存${changes}。`);
    } catch (e) { setError(errorText(e)); } finally { setBusy(false); }
  }
  async function checkUpdates() {
    if (android) { setUpdateError('Android 版暂不支持应用内更新，请使用对应平台的手动安装方式。'); return; }
    setCheckingUpdate(true); setUpdateMessage(''); setUpdateError(''); setUpdateInfo(null); setDownloadedUpdate(false); setDownloadProgress(null);
    try {
      const endpoints = await resolveUpdateEndpoints(updateChannel);
      if (!endpoints.sources.length) {
        throw new Error('当前更新通道没有配置更新源，请编辑 AppData\\Roaming\\NodeSend\\update-sources.json。');
      }
      const update = await checkUpdate(updateChannel, endpoints);
      if (!update) {
        setUpdateMessage(`当前版本 ${displayVersion(await getVersion())}，已是最新版本。`);
        return;
      }
      setUpdateInfo({ ...update, version: displayVersion(update.version), currentVersion: displayVersion(update.currentVersion) });
      setUpdateMessage(`发现新版本 ${displayVersion(update.version)}，当前版本为 ${displayVersion(update.currentVersion)}。`);
    } catch (e) {
      setUpdateError(e instanceof Error ? e.message : errorText(e));
    } finally { setCheckingUpdate(false); }
  }
  async function downloadUpdatePackage() {
    setDownloadingUpdate(true); setUpdateError(''); setDownloadProgress(null);
    try {
      await downloadUpdatePackageApi(setDownloadProgress);
      setDownloadedUpdate(true);
      setUpdateMessage('更新包下载完成，签名验证通过。确认后即可安装。');
    } catch (e) {
      setUpdateError(e instanceof Error ? e.message : errorText(e));
    } finally { setDownloadingUpdate(false); }
  }
  async function installUpdatePackage() {
    if (!window.confirm(`确认安装 NodeSend ${updateInfo?.version ?? '新版本'}？安装时应用将关闭。`)) return;
    setInstallingUpdate(true); setUpdateError('');
    try {
      await installUpdateApi(true);
      setUpdateMessage('安装程序已启动。');
    } catch (e) {
      setUpdateError(e instanceof Error ? e.message : errorText(e));
    } finally { setInstallingUpdate(false); }
  }
  async function openUpdateDownload() {
    if (!updateInfo) return;
    try { await openUpdateUrl(updateInfo.releasePageUrl); }
    catch (e) { setUpdateError(e instanceof Error ? e.message : errorText(e)); }
  }
  return <div className="page"><header className="page-heading"><div><span className="eyebrow">NODESEND / SETTINGS</span><h1>本机与设置</h1><p>设备身份属于这台设备，文件由你掌控。</p></div><div className="page-heading-actions"><SidebarToggle /></div></header>
    {error && <div className="notice error" role="alert">{error}</div>}{message && <div className="notice success" role="status">{message}</div>}
    <section className="surface settings-card"><h2>本机设备</h2><div className="form-stack"><label>设备名称<input value={name} maxLength={64} onChange={e => setName(e.target.value)} /></label><label>默认接收目录<div className="button-row"><input value={android ? directoryLabel : directory} readOnly={android} onChange={e => setDirectory(e.target.value)} /><button type="button" onClick={async () => { try { const path = await chooseReceiveDirectory(); if (path) { setDirectory(path); setMessage('已选择接收目录，请点击“保存设置”使其生效。'); } } catch (e) { setError(errorText(e)); } }}>选择</button></div></label><label className="policy-field">接收策略<div className="policy-picker"><button type="button" className="policy-trigger" aria-haspopup="listbox" aria-expanded={policyOpen} onClick={event => { const rect = event.currentTarget.getBoundingClientRect(); setPolicyFlip(window.innerHeight - rect.bottom < 180); setPolicyOpen(value => !value); }}><span><strong>{selectedPolicy.label}</strong><small>{selectedPolicy.detail}</small></span><Icon name={policyOpen ? 'chevron-up' : 'chevron-down'} size={18} /></button>{policyOpen && <div className={'policy-menu ' + (policyFlip ? 'flip' : '')} role="listbox" aria-label="接收策略选项">{policyOptions.map(option => <button type="button" role="option" aria-selected={option.value === policy} className={'policy-option ' + (option.value === policy ? 'selected' : '')} key={option.value} onClick={() => { setPolicy(option.value); setPolicyOpen(false); }}><span><strong>{option.label}</strong><small>{option.detail}</small></span>{option.value === policy && <Icon name="check" size={17} />}</button>)}</div>}</div></label><div className="update-check-field"><label>检查更新</label><div className="update-check-row"><div className="policy-picker update-channel-picker"><button type="button" className="policy-trigger" aria-haspopup="listbox" aria-expanded={updateChannelOpen} onClick={event => { const rect = event.currentTarget.getBoundingClientRect(); setUpdateChannelFlip(window.innerHeight - rect.bottom < 130); setUpdateChannelOpen(value => !value); }}><span><strong>{updateChannel === 'stable' ? '稳定版' : '测试版'}</strong></span><Icon name={updateChannelOpen ? 'chevron-up' : 'chevron-down'} size={18} /></button>{updateChannelOpen && <div className={'policy-menu ' + (updateChannelFlip ? 'flip' : '')} role="listbox" aria-label="更新通道"><button type="button" role="option" aria-selected={updateChannel === 'stable'} className={'policy-option ' + (updateChannel === 'stable' ? 'selected' : '')} onClick={() => { setUpdateChannel('stable'); setUpdateChannelOpen(false); setUpdateInfo(null); setUpdateMessage(''); setUpdateError(''); setDownloadedUpdate(false); }}><span><strong>稳定版</strong></span>{updateChannel === 'stable' && <Icon name="check" size={17} />}</button><button type="button" role="option" aria-selected={updateChannel === 'test'} className={'policy-option ' + (updateChannel === 'test' ? 'selected' : '')} onClick={() => { setUpdateChannel('test'); setUpdateChannelOpen(false); setUpdateInfo(null); setUpdateMessage(''); setUpdateError(''); setDownloadedUpdate(false); }}><span><strong>测试版</strong></span>{updateChannel === 'test' && <Icon name="check" size={17} />}</button></div>}</div><button type="button" className="primary" disabled={checkingUpdate || downloadingUpdate || installingUpdate} onClick={() => void checkUpdates()}>{checkingUpdate ? '检查中…' : '检查更新'}</button></div>{updateMessage && <p className="hint ok" role="status">{updateMessage}</p>}{updateError && <p className="hint error" role="alert">{updateError}</p>}{updateInfo && <div className="update-release"><div className="update-release-heading"><strong>NodeSend {updateInfo.version}</strong><span>{updateChannel === 'stable' ? '稳定版' : '测试版'}</span></div>{updateInfo.notes && <p className="update-release-notes">{updateInfo.notes}</p>}<div className="button-row update-actions"><button type="button" className="primary" disabled={downloadingUpdate || downloadedUpdate || installingUpdate} onClick={() => void downloadUpdatePackage()}>{downloadingUpdate ? '下载中…' : downloadedUpdate ? '已下载并验证' : '应用内下载'}</button><button type="button" disabled={downloadingUpdate || installingUpdate} onClick={() => void openUpdateDownload()}>浏览器下载安装</button></div>{downloadingUpdate && <div className="update-download-progress"><div className="update-progress-label"><span>正在下载并验证签名</span><span>{downloadProgress?.totalBytes ? `${Math.min(100, Math.round(downloadProgress.downloadedBytes / downloadProgress.totalBytes * 100))}%` : `${((downloadProgress?.downloadedBytes ?? 0) / 1024 / 1024).toFixed(2)} MB`}</span></div><progress max={downloadProgress?.totalBytes || 1} value={downloadProgress?.totalBytes ? Math.min(downloadProgress.downloadedBytes, downloadProgress.totalBytes) : 0} /></div>}{downloadedUpdate && <div className="update-install-area"><p className="hint">安装时应用会关闭。请先完成或暂停正在进行的传输任务。</p>{hasActiveTransfers && <p className="hint error">仍有传输任务进行中，暂不能安装。</p>}<button type="button" className="primary" disabled={installingUpdate || hasActiveTransfers} onClick={() => void installUpdatePackage()}>{installingUpdate ? '正在启动安装…' : '确认并安装更新'}</button></div>}</div>}</div><button className="primary" disabled={busy || !name} onClick={save}>保存设置</button></div><section className="settings-network-details" aria-labelledby="settings-network-title"><h3 id="settings-network-title">身份与网络详情</h3><dl className="detail-list"><dt>Node ID</dt><dd className="mono">{network?.node_id ?? info?.node_id}</dd><dt>QUIC / TCP</dt><dd>{network?.quic_port} / {network?.tcp_port} · TLS 1.3</dd><dt>本机地址</dt><dd>{addresses.join(' · ')}</dd><dt>自动发现</dt><dd>mDNS {network?.discovery.mdns ? '运行中' : '不可用'} · UDP {network?.discovery.udp ? '运行中' : '不可用'}</dd><dt>版本号</dt><dd>{appVersion ? displayBuildVersion(appVersion) : '读取中…'}</dd></dl></section></section>
  </div>;
}
