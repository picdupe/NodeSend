import { useTranslation } from 'react-i18next';

export function MediaLibrarySetting({
  enabled,
  busy,
  onChange,
}: {
  enabled: boolean;
  busy: boolean;
  onChange: (enabled: boolean) => void;
}) {
  const { t } = useTranslation('settings');
  return <label>{t('mediaTitle')}
    <div className="button-row media-library-directory">
      <input value="Pictures / Movies / Music" readOnly aria-label={t('mediaTitle')} />
      <button
        type="button"
        className={'media-library-switch ' + (enabled ? 'enabled' : '')}
        disabled={busy}
        onClick={() => onChange(!enabled)}
      >
        {enabled ? t('mediaDisable') : t('mediaEnable')}
      </button>
    </div>
  </label>;
}
