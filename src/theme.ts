import { emit, listen } from '@tauri-apps/api/event';
import { frontendPreview, invoke } from './api/runtime';

export type AccentMode = 'system' | 'custom';

export interface AccentPreference {
  mode: AccentMode;
  color: string;
}

const STORAGE_KEY = 'nodesend.accent';
const DEFAULT_COLOR = '#6457d8';
let nativeAccentTimer: number | undefined;
let lastNativeAccent = '';
let systemAccentListenerStarted = false;

function normalizeColor(value: string | null | undefined): string {
  if (!value) return `${DEFAULT_COLOR}FF`;
  if (/^#[\da-f]{6}$/i.test(value)) return `${value}FF`.toUpperCase();
  return /^#[\da-f]{8}$/i.test(value) ? value.toUpperCase() : `${DEFAULT_COLOR}FF`;
}

function systemAccentColor(): string {
  if (typeof document === 'undefined' || !CSS.supports('color', 'AccentColor')) return DEFAULT_COLOR;
  const probe = document.createElement('span');
  probe.style.color = 'AccentColor';
  probe.style.position = 'fixed';
  probe.style.visibility = 'hidden';
  document.documentElement.appendChild(probe);
  const value = getComputedStyle(probe).color;
  probe.remove();
  const channels = value.match(/[\d.]+/g)?.map(Number);
  if (!channels || channels.length < 3) return DEFAULT_COLOR;
  return `#${channels.slice(0, 3).map(channel => Math.round(channel).toString(16).padStart(2, '0')).join('')}`.toUpperCase();
}

function contrastColor(hex: string): string {
  const channels = hex.slice(1).match(/[\da-f]{2}/gi)?.map(value => parseInt(value, 16) / 255) ?? [0.39, 0.34, 0.85];
  const luminance = channels.map(value => value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4);
  return 0.2126 * luminance[0] + 0.7152 * luminance[1] + 0.0722 * luminance[2] > 0.205 ? '#111827' : '#ffffff';
}

export function applyAccent(preference: AccentPreference): void {
  const root = document.documentElement;
  const color = normalizeColor(preference.color);
  root.dataset.accentMode = preference.mode;
  root.style.setProperty('--custom-accent', color);
  root.style.setProperty('--custom-accent-contrast', contrastColor(color));
  scheduleNativeAccentUpdate();
  if (preference.mode === 'system' && systemAccentListenerStarted && !frontendPreview) {
    void invoke<string | null>('get_system_accent_color').then(current => {
      if (current && document.documentElement.dataset.accentMode === 'system') applySystemAccentColor(current);
    }).catch(() => undefined);
  }
}

function applySystemAccentColor(color: string): void {
  const normalized = normalizeColor(color);
  const root = document.documentElement;
  root.style.setProperty('--system-accent', normalized);
  root.style.setProperty('--system-accent-contrast', contrastColor(normalized));
  scheduleNativeAccentUpdate();
}

function resolvePrimaryColor(): string {
  const probe = document.createElement('span');
  probe.style.color = 'var(--primary)';
  probe.style.position = 'fixed';
  probe.style.visibility = 'hidden';
  document.documentElement.appendChild(probe);
  const value = getComputedStyle(probe).color;
  probe.remove();
  const channels = value.match(/[\d.]+/g)?.map(Number);
  if (!channels || channels.length < 3) return DEFAULT_COLOR;
  const [red, green, blue] = channels;
  const alpha = channels.length > 3 ? Math.round(Math.max(0, Math.min(1, channels[3])) * 255) : 255;
  return `#${[red, green, blue, alpha].map(channel => Math.round(channel).toString(16).padStart(2, '0')).join('')}`;
}

function scheduleNativeAccentUpdate(): void {
  if (!('__TAURI_INTERNALS__' in window)) return;
  if (nativeAccentTimer !== undefined) window.clearTimeout(nativeAccentTimer);
  nativeAccentTimer = window.setTimeout(() => {
    nativeAccentTimer = undefined;
    const color = resolvePrimaryColor();
    if (color === lastNativeAccent) return;
    lastNativeAccent = color;
    void emit('nodesend-accent-changed', color);
  }, 80);
}

export function readAccentPreference(): AccentPreference {
  try {
    const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? 'null') as Partial<AccentPreference> | null;
    return {
      mode: parsed?.mode === 'custom' ? 'custom' : 'system',
      color: normalizeColor(parsed?.color ?? systemAccentColor()),
    };
  } catch {
    return { mode: 'system', color: systemAccentColor() };
  }
}

export function saveAccentPreference(preference: AccentPreference): void {
  const normalized = { ...preference, color: normalizeColor(preference.color) };
  localStorage.setItem(STORAGE_KEY, JSON.stringify(normalized));
  applyAccent(normalized);
}

export function initializeAccent(): void {
  applyAccent(readAccentPreference());
  if (('__TAURI_INTERNALS__' in window) && !frontendPreview && !systemAccentListenerStarted) {
    systemAccentListenerStarted = true;
    void listen<string>('nodesend-system-accent-changed', event => {
      if (document.documentElement.dataset.accentMode === 'system') applySystemAccentColor(event.payload);
    }).then(() => {
      if (document.documentElement.dataset.accentMode === 'system') {
        return invoke<string | null>('get_system_accent_color').then(color => {
          if (color) applySystemAccentColor(color);
        });
      }
    }).catch(() => undefined);
  }
}
