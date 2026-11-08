import { useEffect, useMemo, useRef, useState } from 'react';
import type { CSSProperties, PointerEvent as ReactPointerEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';

type Hsv = { h: number; s: number; v: number };

const presets = [
  '#ff434b', '#e5475c', '#087fd7', '#029bb7', '#7e7978', '#767676',
  '#d5333a', '#e90b22', '#0361ab', '#347e99', '#615f5b', '#494949',
  '#ed6654', '#e90050', '#9694d6', '#00b6c0', '#697687', '#68777a',
  '#df3600', '#c80050', '#6868d1', '#07888e', '#525c69', '#485257',
  '#bf4c13', '#d6008d', '#8864bb', '#00ad8f', '#557a74', '#64816b',
  '#f65f12', '#d000a5', '#8553b2', '#008874', '#49645e', '#ececec',
  '#ff9200', '#ed009a', '#a600b3', '#00cf72', '#3d7900', '#141414',
  '#ffba00', '#980098', '#740080', '#11803c', '#087800', '#00000000',
];

function hsvToHex({ h, s, v }: Hsv): string {
  const saturation = s / 100;
  const value = v / 100;
  const chroma = value * saturation;
  const x = chroma * (1 - Math.abs(((h / 60) % 2) - 1));
  const m = value - chroma;
  const rgb = h < 60 ? [chroma, x, 0] : h < 120 ? [x, chroma, 0] : h < 180 ? [0, chroma, x] : h < 240 ? [0, x, chroma] : h < 300 ? [x, 0, chroma] : [chroma, 0, x];
  return `#${rgb.map(channel => Math.round((channel + m) * 255).toString(16).padStart(2, '0')).join('')}`;
}

function hexToHsv(hex: string): Hsv {
  const value = hex.replace('#', '');
  const [r, g, b] = [0, 2, 4].map(index => parseInt(value.slice(index, index + 2), 16) / 255);
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const delta = max - min;
  let h = 0;
  if (delta) {
    if (max === r) h = 60 * (((g - b) / delta) % 6);
    else if (max === g) h = 60 * ((b - r) / delta + 2);
    else h = 60 * ((r - g) / delta + 4);
  }
  return { h: (h + 360) % 360, s: max ? (delta / max) * 100 : 0, v: max * 100 };
}

function rgbToHex(rgb: number[]): string {
  return `#${rgb.map(value => Math.max(0, Math.min(255, Math.round(value))).toString(16).padStart(2, '0')).join('')}`;
}

function withAlpha(hex: string, alpha: number): string {
  return `${hex.slice(0, 7)}${Math.max(0, Math.min(255, Math.round(alpha))).toString(16).padStart(2, '0')}`.toUpperCase();
}

export function AccentColorPicker({
  color,
  labels,
  onChange,
}: {
  color: string;
  labels: Record<'spectrum' | 'palette' | 'values' | 'hue' | 'saturation' | 'hex' | 'red' | 'green' | 'blue' | 'alpha' | 'rgb' | 'hsv' | 'brightness' | 'darker' | 'darkest' | 'lighter' | 'lightest' | 'screenPicker', string>;
  onChange: (color: string) => void;
}) {
  const { t } = useTranslation('settings');
  const [picking, setPicking] = useState(false);
  const [pickError, setPickError] = useState(false);
  const [open, setOpen] = useState(false);
  const [tab, setTab] = useState<'spectrum' | 'palette' | 'values'>('spectrum');
  const [valueMode, setValueMode] = useState<'rgb' | 'hsv'>('rgb');
  const [flip, setFlip] = useState(false);
  const [draft, setDraft] = useState(color.toUpperCase());
  const [hsv, setHsv] = useState<Hsv>(() => hexToHsv(color));
  const pickerRef = useRef<HTMLDivElement>(null);
  const lastLocalColor = useRef(color.toUpperCase());
  const shadeAnchor = useRef({ color: color.toUpperCase(), hsv: hexToHsv(color), offset: 0, lastColor: color.toUpperCase() });

  useEffect(() => {
    if (!open) return;
    const dismiss = (event: globalThis.PointerEvent) => {
      if (!pickerRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false);
    };
    document.addEventListener('pointerdown', dismiss);
    document.addEventListener('keydown', closeOnEscape);
    return () => {
      document.removeEventListener('pointerdown', dismiss);
      document.removeEventListener('keydown', closeOnEscape);
    };
  }, [open]);

  useEffect(() => {
    const normalized = color.toUpperCase();
    setDraft(normalized);
    if (lastLocalColor.current !== normalized) setHsv(hexToHsv(normalized));
    lastLocalColor.current = normalized;
  }, [color]);

  const rgb = useMemo(() => draft.match(/[\da-f]{2}/gi)?.slice(0, 3).map(value => parseInt(value, 16)) ?? [0, 0, 0], [draft]);
  const alpha = draft.length >= 9 ? parseInt(draft.slice(7, 9), 16) : 255;
  const opaqueColor = rgbToHex(rgb);
  const darkerColor = hsvToHex({ ...hsv, v: Math.max(0, hsv.v - 12) });
  const darkestColor = hsvToHex({ ...hsv, v: Math.max(0, hsv.v - 24) });
  const lighterColor = hsvToHex({ ...hsv, v: Math.min(100, hsv.v + 12) });
  const lightestColor = hsvToHex({ ...hsv, v: Math.min(100, hsv.v + 24) });
  const hueColor = `hsl(${hsv.h} 100% 50%)`;
  const nativeScreenPicker = '__TAURI_INTERNALS__' in window && /Windows/i.test(navigator.userAgent);
  const screenPickerAvailable = nativeScreenPicker || Boolean((window as Window & { EyeDropper?: unknown }).EyeDropper);

  function choose(next: string) {
    const digits = next.replace('#', '');
    const normalized = `#${(digits.length === 6 ? `${digits}FF` : digits.slice(0, 8)).toUpperCase()}`;
    setDraft(normalized);
    const nextHsv = hexToHsv(normalized);
    setHsv(nextHsv);
    lastLocalColor.current = normalized;
    shadeAnchor.current = { color: normalized, hsv: nextHsv, offset: 0, lastColor: normalized };
    onChange(normalized);
  }

  function chooseHsv(next: Hsv) {
    const clamped = { h: ((next.h % 360) + 360) % 360, s: Math.max(0, Math.min(100, next.s)), v: Math.max(0, Math.min(100, next.v)) };
    setHsv(clamped);
    const value = withAlpha(hsvToHex(clamped), alpha);
    setDraft(value);
    lastLocalColor.current = value;
    shadeAnchor.current = { color: value, hsv: clamped, offset: 0, lastColor: value };
    onChange(value);
  }

  function chooseAlpha(nextAlpha: number) {
    const value = withAlpha(opaqueColor, nextAlpha);
    setDraft(value);
    lastLocalColor.current = value;
    shadeAnchor.current = { color: value, hsv, offset: 0, lastColor: value };
    onChange(value);
  }

  function adjustShade(delta: number) {
    let anchor = shadeAnchor.current;
    if (color.toUpperCase() !== anchor.lastColor) {
      anchor = { color: color.toUpperCase(), hsv, offset: 0, lastColor: color.toUpperCase() };
      shadeAnchor.current = anchor;
    }
    const offset = Math.max(-anchor.hsv.v, Math.min(100 - anchor.hsv.v, anchor.offset + delta));
    const value = offset === 0 ? anchor.color : withAlpha(hsvToHex({ ...anchor.hsv, v: anchor.hsv.v + offset }), alpha);
    anchor.offset = offset;
    anchor.lastColor = value;
    setDraft(value);
    setHsv({ ...anchor.hsv, v: anchor.hsv.v + offset });
    lastLocalColor.current = value;
    onChange(value);
  }

  function updateSpectrum(event: ReactPointerEvent<HTMLDivElement>) {
    const rect = event.currentTarget.getBoundingClientRect();
    chooseHsv({ ...hsv, h: ((event.clientX - rect.left) / rect.width) * 359, s: (1 - (event.clientY - rect.top) / rect.height) * 100 });
  }

  async function pickFromScreen() {
    const EyeDropperApi = (window as Window & { EyeDropper?: new () => { open: () => Promise<{ sRGBHex: string }> } }).EyeDropper;
    if (picking || !screenPickerAvailable) return;
    setPicking(true);
    setPickError(false);
    try {
      const selected = nativeScreenPicker
        ? await invoke<string | null>('pick_screen_color')
        : EyeDropperApi ? (await new EyeDropperApi().open()).sRGBHex : null;
      if (selected) choose(selected);
    } catch (error) {
      // 用户取消取色时保留当前颜色，其他失败仅显示本地化提示。
      if (!(error instanceof DOMException && error.name === 'AbortError')) setPickError(true);
    } finally {
      setPicking(false);
    }
  }

  return <div className="accent-color-picker" ref={pickerRef}>
    <button type="button" className="accent-color-trigger" aria-label={labels.hex} aria-expanded={open} onClick={event => { const rect = event.currentTarget.getBoundingClientRect(); setFlip(window.innerHeight - rect.bottom < 452); setOpen(value => !value); }}>
      <span className="accent-color-preview" style={{ backgroundColor: color }} />
      <span className="accent-color-trigger-value">{color.toUpperCase()}</span>
      <span className="accent-color-chevron">⌃</span>
    </button>
    {open && <div className={'accent-picker-popover' + (flip ? ' flip' : '')}>
      <div className="accent-picker-tabs" role="tablist">
        {(['spectrum', 'palette', 'values'] as const).map(item => <button type="button" role="tab" aria-label={labels[item]} aria-selected={tab === item} className={tab === item ? 'active' : ''} key={item} onClick={() => setTab(item)}>
          {item === 'spectrum' ? <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 3v4l7 12v2h2v-2l7-12V3M4 7h16M8 7l4 5 4-5" /></svg> : item === 'palette' ? <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3a9 9 0 1 0 0 18h1.5a2 2 0 0 0 1.4-3.4 1.6 1.6 0 0 1 1.1-2.8H18a3 3 0 0 0 3-3A9 9 0 0 0 12 3Z" /><path d="M7.5 11h.01M10 7.5h.01M15 7.5h.01M17 11h.01" /></svg> : <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 6h16M4 12h16M4 18h16" /><circle cx="9" cy="6" r="2" /><circle cx="15" cy="12" r="2" /><circle cx="8" cy="18" r="2" /></svg>}
        </button>)}
      </div>
      <div className="accent-picker-content">
      {tab === 'spectrum' && <div className="accent-spectrum-panel">
        <div className="accent-spectrum-row" style={{ '--picker-hue-color': hueColor } as CSSProperties}>
        <input className="accent-vertical-range accent-brightness-range" type="range" min="0" max="100" value={Math.round(hsv.v)} onChange={event => chooseHsv({ ...hsv, v: Number(event.target.value) })} aria-label={labels.brightness} />
        <div className="accent-spectrum" role="slider" aria-label={labels.saturation} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(hsv.s)} tabIndex={0}
          style={{ backgroundImage: 'linear-gradient(to bottom,transparent,#fff),linear-gradient(to right,#f00,#ff0,#0f0,#0ff,#00f,#f0f,#f00)', '--spectrum-x': `${hsv.h / 359 * 100}%`, '--spectrum-y': `${100 - hsv.s}%` } as CSSProperties}
          onPointerDown={event => { event.currentTarget.setPointerCapture(event.pointerId); updateSpectrum(event); }}
          onPointerMove={event => { if (event.buttons === 1) updateSpectrum(event); }}
          onKeyDown={event => { const amount = event.shiftKey ? 10 : 2; if (event.key === 'ArrowLeft') chooseHsv({ ...hsv, h: hsv.h - amount }); if (event.key === 'ArrowRight') chooseHsv({ ...hsv, h: hsv.h + amount }); if (event.key === 'ArrowUp') chooseHsv({ ...hsv, s: hsv.s + amount }); if (event.key === 'ArrowDown') chooseHsv({ ...hsv, s: hsv.s - amount }); }} />
        <input className="accent-vertical-range accent-alpha-vertical" type="range" min="0" max="255" value={alpha} onChange={event => chooseAlpha(Number(event.target.value))} aria-label={labels.alpha} style={{ '--alpha-color': opaqueColor } as CSSProperties} />
        </div>
      </div>}
      {tab === 'palette' && <div className="accent-palette" aria-label={labels.palette}>{presets.map(value => <button type="button" key={value} aria-label={value} aria-pressed={color.toLowerCase() === value.toLowerCase()} style={{ backgroundColor: value }} onClick={() => choose(value)} />)}</div>}
      {tab === 'values' && <div className="accent-values-panel">
        <div className="accent-values-toolbar">
          <div className="accent-value-mode" role="group" aria-label={labels.values}>{(['rgb', 'hsv'] as const).map(mode => <button type="button" key={mode} className={valueMode === mode ? 'active' : ''} aria-pressed={valueMode === mode} onClick={() => setValueMode(mode)}>{labels[mode]}</button>)}</div>
          <label className="accent-hex-row" aria-label={labels.hex}><span>#</span><input aria-label={labels.hex} value={draft.replace('#', '')} maxLength={8} spellCheck={false} onChange={event => { const text = event.target.value.replace(/[^\da-f]/gi, '').slice(0, 8); setDraft(`#${text.toUpperCase()}`); if (text.length === 8) choose(`#${text}`); }} onBlur={() => { if (/^#[\da-f]{6}$/i.test(draft)) choose(draft); else if (!/^#[\da-f]{8}$/i.test(draft)) setDraft(color.toUpperCase()); }} /></label>
        </div>
        {valueMode === 'rgb' ? <div className="accent-channel-list">{([['red', 0, '#000000', '#ff0000'], ['green', 1, '#000000', '#00ff00'], ['blue', 2, '#000000', '#0000ff']] as const).map(([name, index, start, end]) => <div className="accent-channel-row" key={name}>
          <label className="accent-channel-field"><span className="accent-channel-name">{labels[name]}</span><input className="accent-channel-number" type="number" min="0" max="255" value={rgb[index] ?? 0} onChange={event => { const next = [...rgb]; next[index] = Number(event.target.value); choose(withAlpha(rgbToHex(next), alpha)); }} /></label>
          <input className="accent-channel-range" type="range" min="0" max="255" value={rgb[index] ?? 0} style={{ '--channel-start': start, '--channel-end': end } as CSSProperties} aria-label={labels[name]} onChange={event => { const next = [...rgb]; next[index] = Number(event.target.value); choose(withAlpha(rgbToHex(next), alpha)); }} />
        </div>)}</div> : <div className="accent-channel-list">{([['hue', 'h', 359, 'linear-gradient(to right,#f00,#ff0,#0f0,#0ff,#00f,#f0f,#f00)'], ['saturation', 's', 100, `linear-gradient(to right,#888,${hueColor})`], ['brightness', 'v', 100, `linear-gradient(to right,#000,${hueColor})`]] as const).map(([name, key, max, background]) => <div className="accent-channel-row" key={key}>
          <label className="accent-channel-field"><span className="accent-channel-name">{labels[name]}</span><input className="accent-channel-number" type="number" min="0" max={max} value={Math.round(hsv[key])} onChange={event => chooseHsv({ ...hsv, [key]: Number(event.target.value) })} /></label>
          <input className="accent-channel-range" type="range" min="0" max={max} value={Math.round(hsv[key])} style={{ background }} aria-label={labels[name]} onChange={event => chooseHsv({ ...hsv, [key]: Number(event.target.value) })} />
        </div>)}</div>}
        <div className="accent-channel-row accent-alpha-row">
          <label className="accent-channel-field"><span className="accent-channel-name">{labels.alpha}</span><input className="accent-channel-number" type="number" min="0" max="255" value={alpha} onChange={event => choose(withAlpha(opaqueColor, Number(event.target.value)))} /></label>
          <input className="accent-channel-range accent-alpha-range" type="range" min="0" max="255" value={alpha} style={{ '--alpha-color': opaqueColor } as CSSProperties} aria-label={labels.alpha} onChange={event => choose(withAlpha(opaqueColor, Number(event.target.value)))} />
        </div>
      </div>}
      </div>
      <div className="accent-picker-swatch-strip" role="group" aria-label={labels.values}>
        <button type="button" aria-label={labels.darkest} style={{ backgroundColor: darkestColor }} onClick={() => adjustShade(-24)} />
        <button type="button" aria-label={labels.darker} style={{ backgroundColor: darkerColor }} onClick={() => adjustShade(-12)} />
        <span className="accent-picker-current-color" aria-label={draft.toUpperCase()} style={{ backgroundColor: opaqueColor }} />
        <button type="button" aria-label={labels.lighter} style={{ backgroundColor: lighterColor }} onClick={() => adjustShade(12)} />
        <button type="button" aria-label={labels.lightest} style={{ backgroundColor: lightestColor }} onClick={() => adjustShade(24)} />
      </div>
      <button type="button" className="accent-screen-picker" disabled={picking || !screenPickerAvailable} onClick={() => void pickFromScreen()}>{labels.screenPicker}</button>
      {pickError && <span role="alert">{t('accent.screenPickerFailed')}</span>}
    </div>}
  </div>;
}
