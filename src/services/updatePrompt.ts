import type { UpdateChannel, UpdateInfo } from '../api/update';
import { checkAndroidUpdate, checkUpdate } from '../api/update';
import { frontendPreview } from '../api/runtime';
import { resolveUpdateEndpoints } from './updateSources';

const CHANNEL_KEY = 'nodesend.update-channel';
const SKIPPED_KEY = 'nodesend.skipped-update-version';

export const OPEN_UPDATE_PROMPT_EVENT = 'nodesend-open-update-prompt';

export function getStoredUpdateChannel(): UpdateChannel {
  return localStorage.getItem(CHANNEL_KEY) === 'test' ? 'test' : 'stable';
}

export function setStoredUpdateChannel(channel: UpdateChannel) {
  localStorage.setItem(CHANNEL_KEY, channel);
}

/** 跳过的是具体版本，版本号变化后仍会正常提示。 */
export function getSkippedUpdateVersion(): string {
  return localStorage.getItem(SKIPPED_KEY) ?? '';
}

export function setSkippedUpdateVersion(version: string) {
  if (version) localStorage.setItem(SKIPPED_KEY, version);
  else localStorage.removeItem(SKIPPED_KEY);
}

/** 设置页手动检查到更新时，交给全局更新弹窗统一展示。 */
export function openUpdatePrompt(update: UpdateInfo) {
  window.dispatchEvent(new CustomEvent<UpdateInfo>(OPEN_UPDATE_PROMPT_EVENT, { detail: update }));
}

/** 启动/窗口重新打开时的静默检查；失败不打扰用户。 */
export async function findAvailableUpdate(): Promise<UpdateInfo | null> {
  if (frontendPreview) return null;
  const channel = getStoredUpdateChannel();
  if (/Android/i.test(navigator.userAgent)) return checkAndroidUpdate(channel);
  const endpoints = await resolveUpdateEndpoints(channel);
  if (!endpoints.sources.length) return null;
  return checkUpdate(channel, endpoints);
}
