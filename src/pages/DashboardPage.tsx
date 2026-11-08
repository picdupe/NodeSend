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
import { SelectMenu } from '../components/SelectMenu';
import { AccentColorPicker } from '../components/AccentColorPicker';

import { addressPriority } from '../utils/address';
import { displayVersion, displayBuildVersion } from '../utils/version';
import { useTranslation } from 'react-i18next';
import { changeLanguagePreference, getLanguagePreference, type LanguagePreference } from '../i18n';
import { readAccentPreference, saveAccentPreference, type AccentPreference } from '../theme';
import { readAppearancePreference, setAppearancePreference, type AppearancePreference } from '../appearance';

export function DashboardPage({ network, onSaved }: { network: NetworkStatus | null; onSaved: () => Promise<void> }) {
  const { t } = useTranslation('settings');
  const [languagePreference, setLanguagePreference] = useState<LanguagePreference>('system');
  const [accentPreference, setAccentPreference] = useState<AccentPreference>(() => readAccentPreference());
  const [appearancePreference, setAppearance] = useState<AppearancePreference>(() => readAppearancePreference());
  const addresses = [...(network?.addresses ?? [])].sort((a, b) => addressPriority(a) - addressPriority(b));
  const [info, setInfo] = useState<NodeInfo | null>(null); const [error, setError] = useState(''); const [busy, setBusy] = useState(false);
  const [toasts, setToasts] = useState<{ id: number; text: string }[]>([]);
  const toastId = useRef(0);
  const [mediaLibraryBusy, setMediaLibraryBusy] = useState(false);
  const [appVersion, setAppVersion] = useState('');
  const [name, setName] = useState(network?.display_name ?? ''); const [directory, setDirectory] = useState(network?.receive_directory ?? ''); const [policy, setPolicy] = useState(network?.receive_policy ?? 'ask');
  const [saveToGallery, setSaveToGallery] = useState(network?.media_library_enabled ?? false);
  const directoryLabel = useReceiveDirectoryLabel(directory);
  const android = /Android/i.test(navigator.userAgent);
  type SettingsValues = { name: string; directory: string; policy: string };
  const saved = useRef<SettingsValues>({ name: network?.display_name ?? '', directory: network?.receive_directory ?? '', policy: network?.receive_policy ?? 'ask' });
  const latest = useRef<SettingsValues>({ name: network?.display_name ?? '', directory: network?.receive_directory ?? '', policy: network?.receive_policy ?? 'ask' });
  latest.current = { name, directory, policy };
  const saveQueue = useRef<Promise<void>>(Promise.resolve());
  const mounted = useRef(true);
  const initialized = useRef(network !== null);
  const [policyOpen, setPolicyOpen] = useState(false); const [policyFlip, setPolicyFlip] = useState(false);
  const [updateChannel, setUpdateChannel] = useState<'stable' | 'test'>(getStoredUpdateChannel);
  const [updateChannelOpen, setUpdateChannelOpen] = useState(false); const [updateChannelFlip, setUpdateChannelFlip] = useState(false);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [updateMessage, setUpdateMessage] = useState('');
  const [updateError, setUpdateError] = useState('');
  const policyOptions = [{ value: 'ask', label: 'askEveryTime', detail: 'askDescription' }, { value: 'trusted', label: 'trustedOnly', detail: 'trustedDescription' }, { value: 'reject', label: 'autoReject', detail: 'rejectDescription' }] as const;
  const selectedPolicy = policyOptions.find(option => option.value === policy) ?? policyOptions[0];
  useEffect(() => {
    getNodeInfo().then(setInfo).catch(e => setError(errorText(e)));
    getVersion().then(setAppVersion).catch(() => setAppVersion(''));
    getLanguagePreference().then(setLanguagePreference);
  }, []);

  function updateAccent(next: AccentPreference) {
    saveAccentPreference(next);
    setAccentPreference(next);
  }

  function updateAppearance(value: string) {
    const next = value as AppearancePreference;
    setAppearancePreference(next);
    setAppearance(next);
  }

  async function updateLanguage(value: LanguagePreference) {
    try {
      await changeLanguagePreference(value);
      setLanguagePreference(value);
      showToast(t('language.saved'));
    } catch (e) {
      setError(errorText(e));
    }
  }
  useEffect(() => {
    if (!network || initialized.current) return;
    setName(network.display_name); setDirectory(network.receive_directory); setPolicy(network.receive_policy); setSaveToGallery(network.media_library_enabled);
    saved.current = { name: network.display_name, directory: network.receive_directory, policy: network.receive_policy };
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
      if (next.name === before.name && next.directory === before.directory && next.policy === before.policy) return;
      const changedName = next.name !== before.name;
      if (mounted.current) { setBusy(true); setError(''); }
      try {
        await setNetworkSettings(next.name, next.directory, next.policy);
        saved.current = next;
        await onSaved();
        if (mounted.current) showToast(changedName ? t('nameSavedToast') : t('savedToast'));
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
        if (!granted) { setError(t('mediaPermission')); return; }
      }
      await setAndroidMediaLibraryEnabled(enabled);
      setSaveToGallery(enabled);
      await onSaved();
      showToast(enabled ? t('mediaEnabled') : t('mediaDisabled'));
    } catch (e) { setError(errorText(e)); }
    finally { setMediaLibraryBusy(false); }
  }
  async function checkUpdates() {
    setCheckingUpdate(true); setUpdateMessage(''); setUpdateError('');
    try {
      if (frontendPreview) {
        openUpdatePrompt({
          version: '0.1.4-build.126',
          currentVersion: '0.1.4-build.125',
          notes: t('updateDialog.previewNotes'),
          publishedAt: null,
          downloadUrl: '#preview-update',
          releasePageUrl: '#preview-update',
          channel: updateChannel,
        });
        setUpdateMessage(t('updatePreview'));
        return;
      }
      if (android) {
        const update = await checkAndroidUpdate(updateChannel);
        if (!update) { setUpdateMessage(t('androidUpToDate')); return; }
        openUpdatePrompt(update);
        setUpdateMessage(t('updateAvailable', { version: displayVersion(update.version) }));
        return;
      }
      const endpoints = await resolveUpdateEndpoints(updateChannel);
      if (!endpoints.sources.length) {
        throw new Error(t('updateSourceMissing'));
      }
      const update = await checkUpdate(updateChannel, endpoints);
      if (!update) {
        setUpdateMessage(t('upToDate', { version: displayVersion(await getVersion()) }));
        return;
      }
      openUpdatePrompt(update);
      setUpdateMessage(t('updateAvailable', { version: displayVersion(update.version) }));
    } catch (e) {
      setUpdateError(errorText(e));
    } finally { setCheckingUpdate(false); }
  }
  return <div className="page settings-page"><header className="page-heading"><div><span className="eyebrow">NODESEND / SETTINGS</span></div><div className="page-heading-actions"><SidebarToggle /><WindowControls /></div></header>
    {error && <div className="notice error" role="alert">{error}</div>}
    <section className="surface settings-card">{toasts.length > 0 && <div className="save-toast-layer" role="status" aria-live="polite">{toasts.map(item => <div className="save-toast" key={item.id}>{item.text}</div>)}</div>}<h2>{t('device')}</h2><div className="form-stack"><label>{t('name')}<input value={name} maxLength={64} onChange={e => setName(e.target.value)} onBlur={() => persist()} /></label><label>{t('receiveDirectory')}<div className="button-row"><input value={android ? directoryLabel : directory} readOnly={android} onChange={e => setDirectory(e.target.value)} onBlur={android ? undefined : () => persist()} /><button type="button" onClick={async () => { try { const path = await chooseReceiveDirectory(); if (path) { setDirectory(path); persist({ directory: path }); } } catch (e) { setError(errorText(e)); } }}>{t('choose')}</button></div></label>{android && <MediaLibrarySetting enabled={saveToGallery} busy={busy || mediaLibraryBusy} onChange={enabled => void toggleSaveToGallery(enabled)} />}<label className="policy-field">{t('receivePolicy')}<div className="policy-picker"><button type="button" className="policy-trigger" aria-haspopup="listbox" aria-expanded={policyOpen} onClick={event => { const rect = event.currentTarget.getBoundingClientRect(); setPolicyFlip(window.innerHeight - rect.bottom < 180); setPolicyOpen(value => !value); }}><span><strong>{t(selectedPolicy.label)}</strong><small>{t(selectedPolicy.detail)}</small></span><Icon name={policyOpen ? 'chevron-up' : 'chevron-down'} size={18} /></button>{policyOpen && <div className={'policy-menu ' + (policyFlip ? 'flip' : '')} role="listbox" aria-label={t('receivePolicy')}>{policyOptions.map(option => <button type="button" role="option" aria-selected={option.value === policy} className={'policy-option ' + (option.value === policy ? 'selected' : '')} key={option.value} onClick={() => { setPolicy(option.value); setPolicyOpen(false); persist({ policy: option.value }); }}><span><strong>{t(option.label)}</strong><small>{t(option.detail)}</small></span>{option.value === policy && <Icon name="check" size={17} />}</button>)}</div>}</div></label><div className="update-check-field"><label>{t('checkUpdates')}</label><div className="update-check-row"><div className="policy-picker update-channel-picker"><button type="button" className="policy-trigger" aria-haspopup="listbox" aria-expanded={updateChannelOpen} onClick={event => { const rect = event.currentTarget.getBoundingClientRect(); setUpdateChannelFlip(window.innerHeight - rect.bottom < 130); setUpdateChannelOpen(value => !value); }}><span><strong>{t(updateChannel === 'stable' ? 'stable' : 'beta')}</strong></span><Icon name={updateChannelOpen ? 'chevron-up' : 'chevron-down'} size={18} /></button>{updateChannelOpen && <div className={'policy-menu ' + (updateChannelFlip ? 'flip' : '')} role="listbox" aria-label={t('checkUpdates')}><button type="button" role="option" aria-selected={updateChannel === 'stable'} className={'policy-option ' + (updateChannel === 'stable' ? 'selected' : '')} onClick={() => { setUpdateChannel('stable'); setStoredUpdateChannel('stable'); setUpdateChannelOpen(false); setUpdateMessage(''); setUpdateError(''); }}><span><strong>{t('stable')}</strong></span>{updateChannel === 'stable' && <Icon name="check" size={17} />}</button><button type="button" role="option" aria-selected={updateChannel === 'test'} className={'policy-option ' + (updateChannel === 'test' ? 'selected' : '')} onClick={() => { setUpdateChannel('test'); setStoredUpdateChannel('test'); setUpdateChannelOpen(false); setUpdateMessage(''); setUpdateError(''); }}><span><strong>{t('beta')}</strong></span>{updateChannel === 'test' && <Icon name="check" size={17} />}</button></div>}</div><button type="button" className="primary" disabled={checkingUpdate} onClick={() => void checkUpdates()}>{checkingUpdate ? t('checking') : t('checkUpdates')}</button></div>{updateMessage && <p className="hint ok" role="status">{updateMessage}</p>}{updateError && <p className="hint error" role="alert">{updateError}</p>}</div></div><div className="settings-language-field"><label>{t('language.title')}</label><SelectMenu label={t('language.title')} value={languagePreference} options={[{ value: 'system', label: t('language.system') }, { value: 'zh-CN', label: t('language.zhCN') }, { value: 'zh-TW', label: t('language.zhTW') }, { value: 'en-US', label: t('language.enUS') }, { value: 'ja-JP', label: t('language.jaJP') }]} onChange={value => void updateLanguage(value)} /></div><div className="settings-language-field"><label>{t('appearance.title')}</label><SelectMenu label={t('appearance.title')} value={appearancePreference} options={[{ value: 'system', label: t('appearance.system') }, { value: 'light', label: t('appearance.light') }, { value: 'dark', label: t('appearance.dark') }]} onChange={updateAppearance} /></div><div className="settings-language-field settings-accent-field"><label>{t('accent.title')}</label><SelectMenu label={t('accent.title')} value={accentPreference.mode} options={[{ value: 'system', label: t('accent.system') }, { value: 'custom', label: t('accent.custom') }]} onChange={value => updateAccent({ ...accentPreference, mode: value as AccentPreference['mode'] })} />{accentPreference.mode === 'custom' && <div className="accent-color-control"><label>{t('accent.color')}</label><AccentColorPicker color={accentPreference.color} labels={{ spectrum: t('accent.spectrum'), palette: t('accent.palette'), values: t('accent.values'), hue: t('accent.hue'), saturation: t('accent.saturation'), hex: t('accent.hex'), red: t('accent.red'), green: t('accent.green'), blue: t('accent.blue'), alpha: t('accent.alpha'), rgb: t('accent.rgb'), hsv: t('accent.hsv'), brightness: t('accent.brightness'), darker: t('accent.darker'), darkest: t('accent.darkest'), lighter: t('accent.lighter'), lightest: t('accent.lightest'), screenPicker: t('accent.screenPicker') }} onChange={color => updateAccent({ ...accentPreference, color })} /></div>}</div><section className="settings-network-details" aria-labelledby="settings-network-title"><h3 id="settings-network-title">{t('identityNetwork')}</h3><dl className="detail-list"><dt>Node ID</dt><dd className="mono">{network?.node_id ?? info?.node_id}</dd><dt>QUIC / TCP</dt><dd>{network?.quic_port} / {network?.tcp_port} · TLS 1.3</dd><dt>{t('localAddresses')}</dt><dd>{addresses.join(' · ')}</dd><dt>{t('discovery')}</dt><dd>mDNS {network?.discovery.mdns ? t('running') : t('unavailable')} · UDP {network?.discovery.udp ? t('running') : t('unavailable')}</dd><dt>{t('version')}</dt><dd>{appVersion ? displayBuildVersion(appVersion) : t('loading')}</dd></dl></section></section>
  </div>;
}
