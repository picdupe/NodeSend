/**
 * 文件功能：外部 HTTP 共享 API 的浏览器端客户端。
 *
 * - UI 通过本文件"像第三方一样"实际调用内嵌 HTTP 服务（基线 §12），
 *   可直接验证外部调用链路（用户明确要求目录树 API 可用）；
 * - safeFetchJson：先检查 res.ok，JSON 解析失败时携带 URL 与原文报错，
 *   避免服务端返回纯文本（如 404 Not Found）时静默解析崩溃（经验教训）；
 * - 动态片段（路径）一律 encodeURIComponent，固定前缀不编码（经验教训）。
 */
import type { ListResponse, ShareConfig, TreeNode } from '../types';
import i18next from 'i18next';
import { invoke } from './runtime';

/** 由 NodeInfo.http_listen（如 0.0.0.0:58080）得到浏览器可访问的基地址 */
export function toHttpBase(httpListen: string): string {
  const separator = httpListen.lastIndexOf(':');
  const port = separator >= 0 ? httpListen.slice(separator + 1) : httpListen;
  return `http://localhost:${port}`;
}

/** 安全 JSON 请求：错误信息包含请求 URL，便于定位 */
async function safeFetchJson<T>(url: string): Promise<T> {
  const res = await fetch(url);
  if (!res.ok) {
    const text = await res.text().catch(() => '');
    throw new Error(`HTTP ${res.status} (${url}): ${text || res.statusText}`);
  }
  const text = await res.text();
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new Error(i18next.t('generic.invalidResponse', { ns: 'errors' }));
  }
}

/** 追加访问令牌（仅 token 模式需要） */
function withToken(share: ShareConfig, url: string): string {
  if (share.access_mode === 'token' && share.access_token) {
    const sep = url.includes('?') ? '&' : '?';
    return `${url}${sep}token=${encodeURIComponent(share.access_token)}`;
  }
  return url;
}

/**
 * ★调用目录树 API：GET /api/v1/shares/{id}/tree?path=&depth=
 * @param virtualPath 虚拟相对路径，根传空字符串
 * @param depth 0=仅节点；默认 3；上限 20
 */
export async function fetchTree(
  base: string,
  share: ShareConfig,
  virtualPath = '',
  depth = 3,
): Promise<TreeNode> {
  const url = withToken(
    share,
    `${base}/api/v1/shares/${encodeURIComponent(share.id)}/tree` +
      `?path=${encodeURIComponent(virtualPath)}&depth=${depth}`,
  );
  return safeFetchJson<TreeNode>(url);
}

/** 调用单层列表 API：GET /api/v1/shares/{id}/list?path= */
export async function fetchList(
  base: string,
  share: ShareConfig,
  virtualPath = '',
): Promise<ListResponse> {
  const url = withToken(
    share,
    `${base}/api/v1/shares/${encodeURIComponent(share.id)}/list` +
      `?path=${encodeURIComponent(virtualPath)}`,
  );
  return safeFetchJson<ListResponse>(url);
}

/** 生成文件下载直链（浏览器可直接打开） */
export function downloadUrl(
  base: string,
  share: ShareConfig,
  virtualPath: string,
): string {
  return withToken(
    share,
    `${base}/api/v1/shares/${encodeURIComponent(share.id)}/download` +
      `?path=${encodeURIComponent(virtualPath)}`,
  );
}

export interface ShareSearchResult {
  name: string;
  path: string;
  type: 'file' | 'dir';
  size: number;
  modified: string;
}

export interface UploadSession {
  id: string;
  offset: number;
  size: number;
  complete: boolean;
}

export function shareUrl(base: string, share: ShareConfig, endpoint: string, params: Record<string, string> = {}): string {
  const url = new URL(`${base}/api/v1/shares/${encodeURIComponent(share.id)}/${endpoint}`);
  for (const [key, value] of Object.entries(params)) url.searchParams.set(key, value);
  return withToken(share, url.toString());
}

/** Browser directory tree and file actions for a shared folder. */
export function shareBrowserUrl(base: string, share: ShareConfig): string {
  return shareUrl(base, share, 'browser');
}

/** Stable HTTP listener URL; the named path selects the shared directory. */
export function protocolShareBrowserUrl(base: string, share: ShareConfig): string {
  const pathParts = share.path.split(/[\\/]/).filter(Boolean);
  const folderName = pathParts[pathParts.length - 1] || share.name;
  const url = new URL(`${base.replace(/\/+$/, '')}/${encodeURIComponent(share.protocol_share_name || folderName)}`);
  return withToken(share, url.toString());
}

/** Open a share page in the system browser. */
export function openShareUrl(url: string): Promise<void> {
  return invoke<void>('open_external_url', { url });
}

/** Search directory names from the cached tree and files in the current directory. */
export async function searchShare(
  base: string,
  share: ShareConfig,
  query: string,
  type?: 'file' | 'dir',
  limit = 50,
  path = '',
): Promise<ShareSearchResult[]> {
  return safeFetchJson<ShareSearchResult[]>(shareUrl(base, share, 'search', {
    q: query,
    path,
    ...(type ? { type } : {}),
    limit: String(limit),
  }));
}

/** Direct links for browser preview and streamed directory ZIP download. */
export function previewUrl(base: string, share: ShareConfig, path: string): string {
  return shareUrl(base, share, 'preview', { path });
}

export function downloadFolderUrl(base: string, share: ShareConfig, path: string): string {
  return shareUrl(base, share, 'download-folder', { path });
}

/** The server sends tree_changed notifications for this share over WebSocket. */
export function shareEventsUrl(base: string, share: ShareConfig): string {
  const http = shareUrl(base, share, 'events');
  return http.replace(/^http:/, 'ws:').replace(/^https:/, 'wss:');
}

export async function startShareUpload(
  base: string,
  share: ShareConfig,
  name: string,
  size: number,
  directory = '',
  uploadId?: string,
): Promise<UploadSession> {
  const response = await fetch(shareUrl(base, share, 'upload/init'), {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ name, size, directory, upload_id: uploadId ?? null }),
  });
  if (!response.ok) throw new Error(await response.text().catch(() => `HTTP ${response.status}`));
  return await response.json() as UploadSession;
}

export async function getShareUpload(
  base: string,
  share: ShareConfig,
  uploadId: string,
): Promise<UploadSession> {
  return safeFetchJson<UploadSession>(shareUrl(base, share, `upload/${encodeURIComponent(uploadId)}`));
}

/** Upload sequential chunks so a reconnect can resume from the server offset. */
export async function uploadShareFile(
  base: string,
  share: ShareConfig,
  file: File,
  directory = '',
  uploadId?: string,
  onProgress?: (uploaded: number, total: number) => void,
): Promise<UploadSession> {
  const session = await startShareUpload(base, share, file.name, file.size, directory, uploadId);
  if (session.complete) return session;
  const chunkSize = 4 * 1024 * 1024;
  let offset = session.offset;
  while (offset < file.size) {
    const end = Math.min(offset + chunkSize, file.size);
    const response = await fetch(shareUrl(base, share, `upload/${encodeURIComponent(session.id)}`), {
      method: 'PUT',
      headers: { 'content-range': `bytes=${offset}-${end - 1}/${file.size}` },
      body: file.slice(offset, end),
    });
    if (!response.ok) throw new Error(await response.text().catch(() => `HTTP ${response.status}`));
    const next = await response.json() as UploadSession;
    if (next.offset <= offset && !next.complete) throw new Error('Upload made no progress');
    offset = next.offset;
    onProgress?.(offset, file.size);
    if (next.complete) return next;
  }
  return getShareUpload(base, share, session.id);
}
