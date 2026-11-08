import { Channel } from '@tauri-apps/api/core';
import { frontendPreview, invoke } from './runtime';
import i18next from 'i18next';

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
  protocolVersion?: number | null;
  sha256?: string;
  versionCode?: number;
  fallbackUrls?: string[];
}

export interface UpdateDownloadProgress {
  downloadedBytes: number;
  totalBytes: number | null;
  finished: boolean;
}

function requireDesktopRuntime() {
  if (frontendPreview) throw new Error(i18next.t('update.desktopOnly', { ns: 'errors' }));
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
    fallbackUrls?: string[];
    protocolVersion?: number;
  }>('check_android_update', { channel });
  if (!result.updateAvailable) return null;
  if (!result.version || !result.downloadUrl || !result.sha256 || !result.versionCode) {
    throw new Error(i18next.t('update.manifestIncomplete', { ns: 'errors' }));
  }
  return {
    version: result.version,
    currentVersion: result.currentVersion ?? '',
    notes: result.notes ?? null,
    publishedAt: null,
    downloadUrl: result.downloadUrl,
    releasePageUrl: result.downloadUrl,
    channel,
    protocolVersion: result.protocolVersion !== undefined && result.protocolVersion >= 0
      ? result.protocolVersion
      : null,
    sha256: result.sha256,
    versionCode: result.versionCode,
    fallbackUrls: result.fallbackUrls ?? [],
  };
}

export function downloadAndroidUpdate(url: string, sha256: string, fallbackUrls: string[] = []) {
  return invoke<string>('download_android_update', { url, sha256, fallbackUrls });
}

export function installAndroidUpdate(path: string, protocolVersion: number | null = null) {
  return invoke<void>('install_android_update', { path, protocolVersion });
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
