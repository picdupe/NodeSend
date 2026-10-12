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
