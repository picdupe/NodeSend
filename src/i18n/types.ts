export const supportedLanguages = ['zh-CN', 'zh-TW', 'en-US', 'ja-JP'] as const;
export type SupportedLanguage = (typeof supportedLanguages)[number];
export type LanguagePreference = 'system' | SupportedLanguage;
export type TranslationNamespace = 'common' | 'transfer' | 'settings' | 'errors';
