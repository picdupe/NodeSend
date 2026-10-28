import { useState } from 'react';
import type { UpdateDownloadProgress, UpdateInfo } from '../api/update';
import {
  downloadAndroidUpdate,
  downloadUpdate,
  installAndroidUpdate,
  installUpdate,
  openUpdateUrl,
} from '../api/update';
import { frontendPreview } from '../api/runtime';
import { displayVersion } from '../utils/version';

type Phase = 'prompt' | 'downloading' | 'installing' | 'done' | 'error';

export function UpdateDialog({ update, onClose, onSkip }: {
  update: UpdateInfo;
  onClose: () => void;
  onSkip: () => void;
}) {
  const [phase, setPhase] = useState<Phase>('prompt');
  const [progress, setProgress] = useState<UpdateDownloadProgress | null>(null);
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const android = /Android/i.test(navigator.userAgent);
  const busy = phase === 'downloading' || phase === 'installing';
  const percent = progress?.totalBytes
    ? Math.min(100, Math.round(progress.downloadedBytes / progress.totalBytes * 100))
    : null;

  async function applyNow() {
    setPhase('downloading'); setError(''); setMessage(''); setProgress(null);
    try {
      if (frontendPreview) {
        setPhase('done');
        setMessage('预览模式：未下载或安装任何文件。');
        return;
      }
      if (android) {
        if (!update.sha256) throw new Error('更新清单缺少 SHA-256 校验值。');
        const path = await downloadAndroidUpdate(update.downloadUrl, update.sha256, update.fallbackUrls ?? []);
        setPhase('installing');
        await installAndroidUpdate(path);
        setPhase('done');
        setMessage('已交给 Android 系统安装器，请按系统提示完成更新。');
      } else {
        await downloadUpdate(setProgress);
        setPhase('installing');
        await installUpdate(true);
        setPhase('done');
        setMessage('安装程序已启动，应用将关闭以完成更新。');
      }
    } catch (reason) {
      setPhase('error');
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function openReleasePage() {
    try { await openUpdateUrl(update.releasePageUrl); }
    catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
  }

  return <div className="app-modal-backdrop" role="presentation" onMouseDown={event => { if (event.target === event.currentTarget && !busy) onClose(); }}>
    <section className="app-modal update-dialog" role="dialog" aria-modal="true" aria-labelledby="update-dialog-title">
      <span className="eyebrow">NODESEND / UPDATE</span>
      <h2 id="update-dialog-title">发现新版本 {displayVersion(update.version)}</h2>
      <p className="update-dialog-meta">当前版本 {displayVersion(update.currentVersion)} · {update.channel === 'stable' ? '稳定版' : '测试版'}</p>
      <div className="update-dialog-notes">
        <strong>更新内容</strong>
        {update.notes?.trim() ? <p>{update.notes}</p> : <p className="update-dialog-empty">该版本未提供更新说明。</p>}
      </div>
      {phase === 'downloading' && <div className="update-download-progress">
        <div className="update-progress-label"><span>正在下载并验证签名</span><span>{percent !== null ? `${percent}%` : `${((progress?.downloadedBytes ?? 0) / 1024 / 1024).toFixed(2)} MB`}</span></div>
        <progress max={progress?.totalBytes || 1} value={progress?.totalBytes ? Math.min(progress.downloadedBytes, progress.totalBytes) : 0} />
      </div>}
      {message && <p className="hint ok" role="status">{message}</p>}
      {error && <p className="hint error" role="alert">{error}</p>}
      <div className="app-modal-actions">
        {phase === 'prompt' && <>
          <button type="button" onClick={onSkip}>跳过本次更新</button>
          <button type="button" onClick={onClose}>下次提醒</button>
          <button type="button" className="primary" onClick={() => void applyNow()}>立即更新</button>
        </>}
        {busy && <button type="button" disabled>{phase === 'downloading' ? '正在下载…' : '正在启动安装…'}</button>}
        {phase === 'done' && <button type="button" className="primary" onClick={onClose}>完成</button>}
        {phase === 'error' && <>
          {!android && <button type="button" onClick={() => void openReleasePage()}>浏览器下载安装</button>}
          <button type="button" onClick={onClose}>稍后</button>
          <button type="button" className="primary" onClick={() => void applyNow()}>重试</button>
        </>}
      </div>
    </section>
  </div>;
}
