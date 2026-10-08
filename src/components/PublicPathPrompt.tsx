/**
 * 文件功能：公网路径确认弹窗（手动发现与发送共用）。
 *
 * - 原生层默认拒绝非局域网地址，错误信息为 `公网路径需要显式允许`
 *   （见 src-tauri/src/network/transport.rs）；
 * - 前端不常驻"允许公网"开关，只在被拒绝后弹出确认，由用户逐次授权；
 * - 连接与发送共用同一个判定，避免发现设备成功却无法发送。
 */
import { useDialogEnter } from './useDialogEnter';
import { Icon } from './Icon';

/** 原生层拒绝未授权公网路径时的错误标记。 */
const PUBLIC_PATH_HINT = '公网路径需要显式允许';

function messageOf(error: unknown): string {
  if (typeof error === 'object' && error && 'message' in error) return String((error as { message: unknown }).message);
  return String(error);
}

/** 判断一次调用是否因为尚未授权公网路径而被拒绝。 */
export function publicPathRejected(error: unknown): boolean {
  const message = messageOf(error);
  const code = typeof error === 'object' && error && 'code' in error ? String((error as { code: unknown }).code) : '';
  return message.includes(PUBLIC_PATH_HINT) || (code === 'FORBIDDEN' && message.includes('公网'));
}

export function publicPathAddress(error: unknown): string {
  return messageOf(error).split(PUBLIC_PATH_HINT + '：')[1]?.trim() ?? '';
}

export function PublicPathPrompt({ address, busy, error, onCancel, onConfirm }: { address: string; busy: boolean; error: string; onCancel: () => void; onConfirm: () => void }) {
  useDialogEnter(true, () => { if (!busy) onConfirm(); });
  return <div className="modal-backdrop public-path-backdrop" onMouseDown={event => { if (event.target === event.currentTarget && !busy) onCancel(); }}><div className="modal surface public-path-prompt" role="alertdialog" aria-modal="true" aria-labelledby="public-path-title" aria-describedby="public-path-detail"><div className="modal-heading"><div><span className="eyebrow">PUBLIC PATH</span><h2 id="public-path-title">检测到公网连接</h2></div><button type="button" className="icon-button" title="关闭公网提示" aria-label="关闭公网提示" disabled={busy} onClick={onCancel}><Icon name="close" /></button></div><p className="hint" id="public-path-detail">连接候选包含公网地址{address && <> <strong className="mono">{address}</strong></>}。继续将允许尝试该地址。公网地址不一定代表流量经过互联网，实际路径由网络路由决定。</p>{error && <div className="notice error" role="alert">{error}</div>}<div className="modal-actions"><button type="button" disabled={busy} onClick={onCancel}>返回修改地址</button><button type="button" className="primary" disabled={busy} onClick={onConfirm}>允许并使用公网路径</button></div></div></div>;
}
