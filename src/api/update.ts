import { Channel } from '@tauri-apps/api/core';
import { frontendPreview, invoke } from './runtime';

export type UpdateChannel = 'stable' | 'test';

export interface UpdateEndpoints {
  sources: string[];
}

export interface UpdateInfo {
  version: string;
  currentVersion: string;
  notes: string | null;
  publishedAt: string | null;
  downloadUrl: string;
  releasePageUrl: string;
  channel: UpdateChannel;
  sha256?: string;
  versionCode?: number;
}

export interface UpdateDownloadProgress {
  downloadedBytes: number;
  totalBytes: number | null;
  finished: boolean;
}

function requireDesktopRuntime() {
  if (frontendPreview) throw new Error('更新功能仅在桌面应用中可用。');
}

export function checkUpdate(channel: UpdateChannel, endpoints: UpdateEndpoints) {
  requireDesktopRuntime();
  return invoke<UpdateInfo | null>('check_update', {
    channel,
    sourceEndpoints: endpoints.sources,
  });
}

export async function checkAndroidUpdate(channel: UpdateChannel): Promise<(UpdateInfo & { versionCode: number; sha256: string }) | null> {
  const result = await invoke<{
    updateAvailable: boolean;
    version?: string;
    currentVersion?: string;
    notes?: string;
    downloadUrl?: string;
    sha256?: string;
    versionCode?: number;
  }>('check_android_update', { channel });
  if (!result.updateAvailable) return null;
  if (!result.version || !result.downloadUrl || !result.sha256 || !result.versionCode) {
    throw new Error('Android 更新清单内容不完整。');
  }
  return {
    version: result.version,
    currentVersion: result.currentVersion ?? '',
    notes: result.notes ?? null,
    publishedAt: null,
    downloadUrl: result.downloadUrl,
    releasePageUrl: result.downloadUrl,
    channel,
    sha256: result.sha256,
    versionCode: result.versionCode,
  };
}

export function downloadAndroidUpdate(url: string, sha256: string) {
  return invoke<string>('download_android_update', { url, sha256 });
}

export function installAndroidUpdate(path: string) {
  return invoke<void>('install_android_update', { path });
}

export function downloadUpdate(onProgress: (progress: UpdateDownloadProgress) => void) {
  requireDesktopRuntime();
  const channel = new Channel<UpdateDownloadProgress>();
  channel.onmessage = onProgress;
  return invoke<void>('download_update', { onProgress: channel });
}

export function installUpdate(confirmed: boolean) {
  requireDesktopRuntime();
  return invoke<void>('install_update', { confirmed });
}

export function openUpdateUrl(url: string) {
  requireDesktopRuntime();
  return invoke<void>('open_update_url', { url });
}
