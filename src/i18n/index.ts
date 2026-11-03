import i18next from 'i18next';
import { initReactI18next } from 'react-i18next';
import { emit } from '@tauri-apps/api/event';
import { frontendPreview } from '../api/runtime';
import { i18nOptions } from './config';
import { getLanguagePreference, getSystemLanguage, normalizeLanguage, saveLanguagePreference } from './language';
import type { LanguagePreference, SupportedLanguage } from './types';

i18next.use(initReactI18next);

let initialized = false;
export async function initializeI18n(): Promise<void> {
  if (initialized) return;
  const [preference, systemLanguage] = await Promise.all([getLanguagePreference(), getSystemLanguage()]);
  const language = preference === 'system' ? systemLanguage : normalizeLanguage(preference);
  await i18next.init({ ...i18nOptions, lng: language });
  if (typeof document !== 'undefined') document.documentElement.lang = language;
  initialized = true;
  if (!frontendPreview) await emit('nodesend-language-changed', language);
}

export async function changeLanguagePreference(preference: LanguagePreference): Promise<void> {
  await saveLanguagePreference(preference);
  const language: SupportedLanguage = preference === 'system' ? await getSystemLanguage() : normalizeLanguage(preference);
  await i18next.changeLanguage(language);
  if (typeof document !== 'undefined') document.documentElement.lang = language;
  if (!frontendPreview) await emit('nodesend-language-changed', language);
}

export { i18next };
export { getLanguagePreference } from './language';
export * from './types';
