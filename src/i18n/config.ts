import type { InitOptions } from 'i18next';
import zhCNCommon from './locales/zh-CN/common.json';
import zhCNTransfer from './locales/zh-CN/transfer.json';
import zhCNSettings from './locales/zh-CN/settings.json';
import zhCNErrors from './locales/zh-CN/errors.json';
import zhTWCommon from './locales/zh-TW/common.json';
import zhTWTransfer from './locales/zh-TW/transfer.json';
import zhTWSettings from './locales/zh-TW/settings.json';
import zhTWErrors from './locales/zh-TW/errors.json';
import enUSCommon from './locales/en-US/common.json';
import enUSTransfer from './locales/en-US/transfer.json';
import enUSSettings from './locales/en-US/settings.json';
import enUSErrors from './locales/en-US/errors.json';
import jaJPCommon from './locales/ja-JP/common.json';
import jaJPTransfer from './locales/ja-JP/transfer.json';
import jaJPSettings from './locales/ja-JP/settings.json';
import jaJPErrors from './locales/ja-JP/errors.json';

export const resources = {
  'zh-CN': { common: zhCNCommon, transfer: zhCNTransfer, settings: zhCNSettings, errors: zhCNErrors },
  'zh-TW': { common: zhTWCommon, transfer: zhTWTransfer, settings: zhTWSettings, errors: zhTWErrors },
  'en-US': { common: enUSCommon, transfer: enUSTransfer, settings: enUSSettings, errors: enUSErrors },
  'ja-JP': { common: jaJPCommon, transfer: jaJPTransfer, settings: jaJPSettings, errors: jaJPErrors },
} as const;

export const i18nOptions: InitOptions = {
  resources,
  fallbackLng: 'zh-CN',
  supportedLngs: Object.keys(resources),
  defaultNS: 'common',
  ns: ['common', 'transfer', 'settings', 'errors'],
  interpolation: { escapeValue: false },
  returnNull: false,
};
