export type AccentMode = 'system' | 'custom';

export interface AccentPreference {
  mode: AccentMode;
  color: string;
}

const STORAGE_KEY = 'nodesend.accent';
const DEFAULT_COLOR = '#6457d8';

function normalizeColor(value: string | null | undefined): string {
  if (!value) return `${DEFAULT_COLOR}FF`;
  if (/^#[\da-f]{6}$/i.test(value)) return `${value}FF`.toUpperCase();
  return /^#[\da-f]{8}$/i.test(value) ? value.toUpperCase() : `${DEFAULT_COLOR}FF`;
}

function contrastColor(hex: string): string {
  const channels = hex.slice(1).match(/[\da-f]{2}/gi)?.map(value => parseInt(value, 16) / 255) ?? [0.39, 0.34, 0.85];
  const luminance = channels.map(value => value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4);
  return 0.2126 * luminance[0] + 0.7152 * luminance[1] + 0.0722 * luminance[2] > 0.179 ? '#111827' : '#ffffff';
}

export function applyAccent(preference: AccentPreference): void {
  const root = document.documentElement;
  const color = normalizeColor(preference.color);
  root.dataset.accentMode = preference.mode;
  root.style.setProperty('--custom-accent', color);
  root.style.setProperty('--custom-accent-contrast', contrastColor(color));
}

export function readAccentPreference(): AccentPreference {
  try {
    const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? 'null') as Partial<AccentPreference> | null;
    return {
      mode: parsed?.mode === 'custom' ? 'custom' : 'system',
      color: normalizeColor(parsed?.color),
    };
  } catch {
    return { mode: 'system', color: DEFAULT_COLOR };
  }
}

export function saveAccentPreference(preference: AccentPreference): void {
  const normalized = { ...preference, color: normalizeColor(preference.color) };
  localStorage.setItem(STORAGE_KEY, JSON.stringify(normalized));
  applyAccent(normalized);
}

export function initializeAccent(): void {
  applyAccent(readAccentPreference());
}
