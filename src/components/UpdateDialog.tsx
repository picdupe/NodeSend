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
import { useTranslation } from 'react-i18next';
import { errorText } from '../api/network';

type Phase = 'prompt' | 'downloading' | 'installing' | 'done' | 'error';

export function UpdateDialog({ update, onClose, onSkip }: {
  update: UpdateInfo;
  onClose: () => void;
  onSkip: () => void;
}) {
  const { t } = useTranslation('settings');
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
        setMessage(t('updateDialog.previewDone'));
        return;
      }
      if (android) {
        if (!update.sha256) {
          setPhase('error');
          setError(t('updateDialog.missingHash'));
          return;
        }
        const path = await downloadAndroidUpdate(update.downloadUrl, update.sha256, update.fallbackUrls ?? []);
        setPhase('installing');
        await installAndroidUpdate(path);
        setPhase('done');
        setMessage(t('updateDialog.androidInstall'));
      } else {
        await downloadUpdate(setProgress);
        setPhase('installing');
        await installUpdate(true);
        setPhase('done');
        setMessage(t('updateDialog.installerStarted'));
      }
    } catch (reason) {
      setPhase('error');
      setError(errorText(reason));
    }
  }

  async function openReleasePage() {
    try { await openUpdateUrl(update.releasePageUrl); }
    catch (reason) { setError(errorText(reason)); }
  }

  return <div className="app-modal-backdrop" role="presentation" onMouseDown={event => { if (event.target === event.currentTarget && !busy) onClose(); }}>
    <section className="app-modal update-dialog" role="dialog" aria-modal="true" aria-labelledby="update-dialog-title">
      <span className="eyebrow">NODESEND / UPDATE</span>
      <h2 id="update-dialog-title">{t('updateDialog.found', { version: displayVersion(update.version) })}</h2>
      <p className="update-dialog-meta">{t('updateDialog.current', { version: displayVersion(update.currentVersion) })} · {update.channel === 'stable' ? t('updateDialog.stable') : t('updateDialog.beta')}</p>
      <div className="update-dialog-notes">
        <strong>{t('updateDialog.notes')}</strong>
        {update.notes?.trim() ? <p>{update.notes}</p> : <p className="update-dialog-empty">{t('updateDialog.noNotes')}</p>}
      </div>
      {phase === 'downloading' && <div className="update-download-progress">
        <div className="update-progress-label"><span>{t('updateDialog.downloadVerify')}</span><span>{percent !== null ? `${percent}%` : `${((progress?.downloadedBytes ?? 0) / 1024 / 1024).toFixed(2)} MB`}</span></div>
        <progress max={progress?.totalBytes || 1} value={progress?.totalBytes ? Math.min(progress.downloadedBytes, progress.totalBytes) : 0} />
      </div>}
      {message && <p className="hint ok" role="status">{message}</p>}
      {error && <p className="hint error" role="alert">{error}</p>}
      <div className="app-modal-actions">
        {phase === 'prompt' && <>
          <button type="button" onClick={onSkip}>{t('updateDialog.skip')}</button>
          <button type="button" onClick={onClose}>{t('updateDialog.remindLater')}</button>
          <button type="button" className="primary" onClick={() => void applyNow()}>{t('updateDialog.updateNow')}</button>
        </>}
        {busy && <button type="button" disabled>{phase === 'downloading' ? t('updateDialog.downloading') : t('updateDialog.installing')}</button>}
        {phase === 'done' && <button type="button" className="primary" onClick={onClose}>{t('updateDialog.done')}</button>}
        {phase === 'error' && <>
          {!android && <button type="button" onClick={() => void openReleasePage()}>{t('updateDialog.browserInstall')}</button>}
          <button type="button" onClick={onClose}>{t('updateDialog.later')}</button>
          <button type="button" className="primary" onClick={() => void applyNow()}>{t('updateDialog.retry')}</button>
        </>}
      </div>
    </section>
  </div>;
}
