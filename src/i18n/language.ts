import { locale as osLocale } from '@tauri-apps/plugin-os';
import { invoke } from '../api/runtime';
import type { LanguagePreference, SupportedLanguage } from './types';
import { supportedLanguages } from './types';

export function normalizeLanguage(input: string | null | undefined): SupportedLanguage {
  const value = (input ?? '').replace('_', '-').toLowerCase();
  if (value === 'zh-tw' || value === 'zh-hk' || value === 'zh-mo' || value.includes('hant')) return 'zh-TW';
  if (value.startsWith('zh')) return 'zh-CN';
  if (value.startsWith('en')) return 'en-US';
  if (value.startsWith('ja')) return 'ja-JP';
  return 'zh-CN';
}

export async function getSystemLanguage(): Promise<SupportedLanguage> {
  try {
    return normalizeLanguage(await osLocale());
  } catch {
    return normalizeLanguage(typeof navigator === 'undefined' ? 'zh-CN' : navigator.language);
  }
}

export async function getLanguagePreference(): Promise<LanguagePreference> {
  try {
    const value = await invoke<string>('get_ui_language');
    return value === 'system' || supportedLanguages.includes(value as SupportedLanguage)
      ? value as LanguagePreference
      : 'system';
  } catch {
    return 'system';
  }
}

export async function saveLanguagePreference(value: LanguagePreference): Promise<void> {
  await invoke<void>('set_ui_language', { language: value });
}
