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

/** 由 NodeInfo.http_listen（如 0.0.0.0:58080）得到浏览器可访问的基地址 */
export function toHttpBase(httpListen: string): string {
  // SocketAddr::to_string() uses `[::1]:port` for IPv6, so taking the
  // second colon-separated segment produces an empty/incorrect port.
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
    throw new Error(`响应不是合法 JSON (${url}): ${text.slice(0, 200)}`);
  }
}

/** 追加访问令牌（仅 token 模式需要） */
function withToken(share: ShareConfig, url: string): string {
  if (share.access_mode === 'token' && share.access_token) {
    const sep = url.includes('?') ? '&' : '?';
    return `${url}${sep}access_token=${encodeURIComponent(share.access_token)}`;
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
