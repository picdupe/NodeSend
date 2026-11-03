import { useReceiveDirectoryLabel } from '../../hooks/useReceiveDirectoryLabel';
import { useEffect, useRef, useState } from 'react';
import type { ConflictPolicy, FileEntry, NetworkStatus, TransferTask } from '../../types/network';
import { chooseReceiveDirectory, errorText, listTaskEntries } from '../../api/network';
import { useReceiveDecision } from '../../hooks/useReceiveDecision';
import { Icon } from '../Icon';
import { localDeviceIcon } from '../../utils/device';
import { formatBytes } from './format';
import { useTranslation } from 'react-i18next';
/** 清单分页大小：清单可能达百万条，必须按需分页加载，不能一次传入前端。 */
const PAGE_SIZE = 200;
function RequestCard({ task, directory, decideReceive, busy, decisionError }: { task: TransferTask; directory: string; decideReceive: ReturnType<typeof useReceiveDecision>['decide']; busy: boolean; decisionError: string }) {
  const { t } = useTranslation('transfer');
  const [destination, setDestination] = useState(directory);
  const manuallySelected = useRef(false);
  useEffect(() => { if (!manuallySelected.current) setDestination(directory); }, [directory]);
  const destinationLabel = useReceiveDirectoryLabel(destination);
  const [policy, setPolicy] = useState<ConflictPolicy>('rename');
  // 默认接收全部（`selected=null`）；仅当用户切到「选择部分」时才逐页挑选。
  const [all, setAll] = useState(true);
  const [selected, setSelected] = useState<string[]>([]);
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [total, setTotal] = useState(task.file_count);
  const [loading, setLoading] = useState(false);
  const [trustDevice, setTrustDevice] = useState(false);
  const [error, setError] = useState('');
  async function choose() { try { const path = await chooseReceiveDirectory(); if (path) { manuallySelected.current = true; setDestination(path); } } catch (e) { setError(errorText(e)); } }
  /** 追加加载一页清单明细（首次加载 offset=0）。 */
  async function loadMore(from: number) {
    setLoading(true);
    try {
      const page = await listTaskEntries(task.id, from, PAGE_SIZE);
      setTotal(page.total);
      setEntries(old => [...old, ...page.entries]);
    } catch (e) { setError(errorText(e)); } finally { setLoading(false); }
  }
  async function decide(accepted: boolean) { try { await decideReceive(task.id, accepted, destination, policy, all ? null : selected, accepted && trustDevice); } catch (e) { setError(errorText(e)); } }
  const files = Math.max(0, task.file_count - task.directory_count);
  const rootsReady = task.top_level_count === 0 || task.file_count >= task.top_level_count;
  return <article className="surface request-card">
    <div className="request-heading"><span className="device-icon"><Icon name="receive" /></span><div><h3>{t('receive.requestTitle', { name: task.peer_name })}</h3><p>{t('send.fileCount', { count: files })}{task.directory_count > 0 ? ' · ' + t('send.directoryCount', { count: task.directory_count }) : ''} · {task.scan_complete ? formatBytes(task.total_bytes) : '-- · ' + t('receive.scanning')} · {t('receive.identityVerified')}</p></div></div>
    <p className="caption mono">{t('send.nodeId')}：{task.peer_id}</p>
    <details onToggle={event => { if ((event.target as HTMLDetailsElement).open && !all && !entries.length && !loading) void loadMore(0); }}>
      <summary>{t('receive.chooseContents')}</summary>
      <div className="receive-options"><div className="segments" role="group" aria-label={t('receive.scope')}><button type="button" aria-pressed={all} className={all ? 'active' : ''} onClick={() => setAll(true)}>{t('receive.all')}</button><button type="button" aria-pressed={!all} className={!all ? 'active' : ''} onClick={() => { setAll(false); if (!entries.length) void loadMore(0); }}>{t('receive.selectSome')}</button></div></div>
      {!all && <div className="request-files">
        {entries.map(e => <label className="check-label" key={e.path}><input type="checkbox" checked={selected.includes(e.path)} onChange={event => setSelected(old => event.target.checked ? [...old, e.path] : old.filter(p => p !== e.path))} /><span className="check-mark" aria-hidden="true"><Icon name="check" size={14} /></span><span>{e.path}</span><small>{e.directory ? t('receive.directory') : formatBytes(e.size)}</small></label>)}
        {!entries.length && !loading && <p className="caption">{t('receive.noEntries')}</p>}
        {entries.length < total && <button type="button" className="text-button" disabled={loading} onClick={() => void loadMore(entries.length)}>{loading ? t('receive.loading') : t('receive.loadMore', { shown: entries.length, total })}</button>}
      </div>}
    </details>
    <div className="receive-options"><div><label>{t('receive.saveTo')}</label><button className="folder-button" onClick={choose} title={destinationLabel}><Icon name="folder" size={16} /><span>{destinationLabel}</span><span>{t('receive.change')}</span></button></div><label>{t('receive.conflicts')}<select value={policy} onChange={e => setPolicy(e.target.value as ConflictPolicy)}><option value="rename">{t('receive.keepBoth')}</option><option value="skip">{t('receive.skipExisting')}</option><option value="overwrite">{t('receive.overwrite')}</option></select></label></div>
    {policy === 'overwrite' && <p className="caption">{t('receive.overwriteWarning')}</p>}
    <label className="check-label"><input type="checkbox" checked={trustDevice} disabled={busy} onChange={event => setTrustDevice(event.target.checked)} /><span className="check-mark" aria-hidden="true"><Icon name="check" size={14} /></span><span>{t('receive.trustDevice')}</span></label>
    {(error || decisionError) && <div className="notice error" role="alert">{error || decisionError}</div>}
    {!rootsReady && <p className="caption">{t('receive.rootsPending')}</p>}
    <div className="request-actions"><button className="danger" disabled={busy} onClick={() => decide(false)}>{t('receive.decline')}</button><button className="primary" disabled={busy || !rootsReady || (!all && !selected.length)} onClick={() => decide(true)}><Icon name="check" size={17} />{t('receive.confirm')}</button></div>
  </article>;
}
export function ReceivePanel({ status, refresh }: { status: NetworkStatus; refresh: () => Promise<void> }) {
  const { t } = useTranslation('transfer');
  const decision = useReceiveDecision(refresh);
  const requests = status.tasks.filter(t => t.direction === 'receive' && t.status === 'waiting');
  const [copied, setCopied] = useState(false);
  return <>
    <section className="receive-identity surface"><div className="identity-mark"><Icon name={localDeviceIcon()} size={34} /></div><div><h2>{status.display_name}</h2><span className="badge success"><span className="status-dot online" />{t('receive.waitingForNodes')}</span></div><button className="copy-node-id" onClick={async () => { try { await navigator.clipboard.writeText(status.node_id); setCopied(true); } catch { setCopied(false); } }}>{copied ? t('receive.copiedNodeId') : t('receive.copyNodeId')}</button></section>
    <div className="section-heading"><h2>{t('receive.requests')} <span className="count-tag">{requests.length}</span></h2></div>
    {!requests.length && <section className="surface empty-state generous"><Icon name="receive" size={38} /></section>}
    {requests.map(task => <RequestCard key={task.id} task={task} directory={status.receive_directory} decideReceive={decision.decide} busy={decision.busy} decisionError={decision.error} />)}
  </>;
}
