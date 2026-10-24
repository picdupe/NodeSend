export function MediaLibrarySetting({
  enabled,
  busy,
  onChange,
}: {
  enabled: boolean;
  busy: boolean;
  onChange: (enabled: boolean) => void;
}) {
  return <label>默认媒体目录
    <div className="button-row media-library-directory">
      <input value="Pictures / Movies / Music" readOnly aria-label="默认媒体目录" />
      <button
        key={enabled ? 'enabled' : 'disabled'}
        type="button"
        className={'media-library-switch ' + (enabled ? 'enabled' : '')}
        disabled={busy}
        onClick={() => onChange(!enabled)}
      >
        {enabled ? '关闭' : '启用'}
      </button>
    </div>
  </label>;
}
