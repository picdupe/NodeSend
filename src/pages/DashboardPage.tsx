import { useReceiveDirectoryLabel } from '../hooks/useReceiveDirectoryLabel';
import { useEffect, useRef, useState } from 'react';
import { getNodeInfo } from '../api/ipc';
import { chooseReceiveDirectory, setNetworkSettings, errorText } from '../api/network';
import type { NodeInfo } from '../types';
import type { NetworkStatus } from '../types/network';
import { Icon } from '../components/Icon';
import { SidebarToggle } from '../components/AppLayout';

import { addressPriority } from '../utils/address';

export function DashboardPage({ network, onSaved }: { network: NetworkStatus | null; onSaved: () => Promise<void> }) {
  const addresses = [...(network?.addresses ?? [])].sort((a, b) => addressPriority(a) - addressPriority(b));
  const [info, setInfo] = useState<NodeInfo | null>(null); const [error, setError] = useState(''); const [message, setMessage] = useState(''); const [busy, setBusy] = useState(false);
  const [name, setName] = useState(network?.display_name ?? ''); const [directory, setDirectory] = useState(network?.receive_directory ?? ''); const [policy, setPolicy] = useState(network?.receive_policy ?? 'ask');
  const directoryLabel = useReceiveDirectoryLabel(directory);
  const android = /Android/i.test(navigator.userAgent);
  const saved = useRef({ name: network?.display_name ?? '', directory: network?.receive_directory ?? '', policy: network?.receive_policy ?? 'ask' });
  const initialized = useRef(network !== null);
  const [policyOpen, setPolicyOpen] = useState(false); const [policyFlip, setPolicyFlip] = useState(false);
  const policyOptions = [{ value: 'ask', label: '每次询问（默认）', detail: '收到文件前确认一次' }, { value: 'trusted', label: '仅自动接收已信任设备', detail: '信任设备可直接保存' }, { value: 'reject', label: '自动拒绝', detail: '所有接收请求都拒绝' }] as const;
  const selectedPolicy = policyOptions.find(option => option.value === policy) ?? policyOptions[0];
  useEffect(() => { getNodeInfo().then(setInfo).catch(e => setError(errorText(e))); }, []);
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
  return <div className="page"><header className="page-heading"><div><span className="eyebrow">NODESEND / SETTINGS</span><h1>本机与设置</h1><p>设备身份属于这台设备，文件由你掌控。</p></div><div className="page-heading-actions"><SidebarToggle /></div></header>
    {error && <div className="notice error" role="alert">{error}</div>}{message && <div className="notice success" role="status">{message}</div>}
    <section className="surface settings-card"><h2>本机设备</h2><div className="form-stack"><label>设备名称<input value={name} maxLength={64} onChange={e => setName(e.target.value)} /></label><label>默认接收目录<div className="button-row"><input value={android ? directoryLabel : directory} readOnly={android} onChange={e => setDirectory(e.target.value)} /><button type="button" onClick={async () => { try { const path = await chooseReceiveDirectory(); if (path) { setDirectory(path); setMessage('已选择接收目录，请点击“保存设置”使其生效。'); } } catch (e) { setError(errorText(e)); } }}>选择</button></div></label><label className="policy-field">接收策略<div className="policy-picker"><button type="button" className="policy-trigger" aria-haspopup="listbox" aria-expanded={policyOpen} onClick={event => { const rect = event.currentTarget.getBoundingClientRect(); setPolicyFlip(window.innerHeight - rect.bottom < 180); setPolicyOpen(value => !value); }}><span><strong>{selectedPolicy.label}</strong><small>{selectedPolicy.detail}</small></span><Icon name={policyOpen ? 'chevron-up' : 'chevron-down'} size={18} /></button>{policyOpen && <div className={'policy-menu ' + (policyFlip ? 'flip' : '')} role="listbox" aria-label="接收策略选项">{policyOptions.map(option => <button type="button" role="option" aria-selected={option.value === policy} className={'policy-option ' + (option.value === policy ? 'selected' : '')} key={option.value} onClick={() => { setPolicy(option.value); setPolicyOpen(false); }}><span><strong>{option.label}</strong><small>{option.detail}</small></span>{option.value === policy && <Icon name="check" size={17} />}</button>)}</div>}</div></label><button className="primary" disabled={busy || !name} onClick={save}>保存设置</button></div><section className="settings-network-details" aria-labelledby="settings-network-title"><h3 id="settings-network-title">身份与网络详情</h3><dl className="detail-list"><dt>Node ID</dt><dd className="mono">{network?.node_id ?? info?.node_id}</dd><dt>QUIC / TCP</dt><dd>{network?.quic_port} / {network?.tcp_port} · TLS 1.3</dd><dt>本机地址</dt><dd>{addresses.join(' · ')}</dd><dt>自动发现</dt><dd>mDNS {network?.discovery.mdns ? '运行中' : '不可用'} · UDP {network?.discovery.udp ? '运行中' : '不可用'}</dd></dl></section></section>
  </div>;
}
