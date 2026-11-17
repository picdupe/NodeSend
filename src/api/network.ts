import { invoke } from './runtime';
import i18next from 'i18next';
import type { ConflictPolicy, NetworkStatus, Peer, PreviewStatus, TaskEntriesPage } from '../types/network';
export const getNetworkStatus = () => invoke<NetworkStatus>('get_network_status');
export const chooseSendFiles = (directory = false, kind: 'files' | 'media' | 'apps' = 'files') => invoke<string[]>('choose_send_files', { directory, kind });
export const takeSharedFiles = () => invoke<string[]>('take_shared_files');
export const stageTextPayload = (text: string) => invoke<string>('stage_text_payload', { text });
export const stageBinaryPayload = (bytes: number[], extension: string) => invoke<string>('stage_binary_payload', { bytes, extension });
export const readClipboardFiles = () => invoke<string[]>('read_clipboard_files');
export const chooseReceiveDirectory = () => invoke<string | null>('choose_receive_directory');
export const previewTransfer = (paths: string[]) => invoke<PreviewStatus>('preview_transfer', { paths });
export const previewStatus = () => invoke<PreviewStatus | null>('preview_status');
export const previewCancel = () => invoke<void>('preview_cancel');
export const listTaskEntries = (id: string, offset: number, limit: number) => invoke<TaskEntriesPage>('list_task_entries', { id, offset, limit });
export const sendFiles = (peerId: string, paths: string[], verifyHash: boolean, allowPublic: boolean, preferredAddress?: string) => invoke<string>('send_files', { peerId, paths, verifyHash, allowPublic, preferredAddress });
export const cancelSendPreflight = (peerId: string) => invoke<void>('cancel_send_preflight', { peerId });
export const addEndpoint = (address: string, expectedNodeId: string | null, allowPublic: boolean) => invoke<Peer>('add_endpoint', { address, expectedNodeId, allowPublic });
export const removePeer = (nodeId: string) => invoke<void>('remove_peer', { nodeId });
export const decideIncoming = (id: string, accepted: boolean, destination: string | null, conflict: ConflictPolicy, selected: string[] | null = null, trustDevice = false) => invoke<void>('decide_incoming_transfer', { id, accepted, destination, conflict, selected, trustDevice });
export const transferAction = (id: string, action: string) => invoke<void>('transfer_action', { id, action });
export const setNetworkSettings = (name: string, directory: string, policy: string) => invoke<void>('set_network_settings', { name, directory, policy });
export const setAndroidMediaLibraryEnabled = (enabled: boolean) => invoke<void>('set_android_media_library_enabled', { enabled });
export const requestMediaLibraryPermission = () => invoke<boolean>('request_media_library_permission');
export function errorText(error: unknown): string {
  let detail: unknown = error;
  if (typeof detail === 'string' && detail.startsWith('{')) {
    try { detail = JSON.parse(detail); } catch { /* preserve the original error */ }
  }
  const raw = typeof detail === 'object' && detail && 'message' in detail ? String(detail.message) : String(detail);
  const code = typeof detail === 'object' && detail && 'code' in detail ? String(detail.code) : '';
  if (raw.includes('SHARE_PATH_PROTOCOL_EXISTS')) {
    return i18next.t('shares.duplicatePathProtocol', { ns: 'errors' });
  }
  if (raw.includes('SMB_USERNAME_PASSWORD_CONFLICT')) {
    return i18next.t('shares.smbCredentialConflict', { ns: 'errors' });
  }
  if (raw.includes('BROWSER_CONNECTION_CHANGED_DURING_REOPEN')) {
    return i18next.t('shares.connectionChangedDuringReopen', { ns: 'errors' });
  }
  if (/At least one bind IP is required/i.test(raw)) {
    return i18next.t('shares.bindIpRequired', { ns: 'common' });
  }
  if (/A wildcard bind IP cannot be combined with a specific IP of the same family/i.test(raw)) {
    return i18next.t('shares.bindIpWildcardConflict', { ns: 'errors' });
  }
  const invalidBindIp = raw.match(/Invalid bind IP address:\s*(.+)$/i);
  if (invalidBindIp) {
    return i18next.t('shares.invalidListenIp', { ns: 'errors', ip: invalidBindIp[1] });
  }
  if (/The update signing public key is not configured/i.test(raw)) {
    return i18next.t('update.unsignedBuild', { ns: 'errors' });
  }
  if (raw === 'UPDATE_BLOCKED_UNFINISHED_TRANSFERS') {
    return i18next.t('settings.unfinishedTransfers', { ns: 'errors' });
  }
  if (raw === 'UPDATE_BLOCKED_ACTIVE_TRANSFERS') {
    return i18next.t('settings.pauseTransfers', { ns: 'errors' });
  }
  const sourceIssues = [
    ['The source file changed. Resuming will retransmit the current version', 'sourceChangedResume'],
    ['The source file is missing. You can finish with the received files or cancel', 'sourceMissingResume'],
    ['The source file is temporarily unavailable. Resume to try again', 'sourceUnavailableResume'],
  ] as const;
  for (const [message, key] of sourceIssues) {
    const prefix = `${message}: `;
    if (raw.startsWith(prefix)) {
      return i18next.t(`transfer.${key}`, { ns: 'errors', file: raw.slice(prefix.length) });
    }
  }
  const connectionDetailPrefix = 'Could not connect to this device. Make sure NodeSend is open on the other device and both devices are on the same network. Connection detail: ';
  if (raw.startsWith(connectionDetailPrefix)) {
    return i18next.t('transfer.connectionUnreachableDetail', {
      ns: 'errors',
      detail: raw.slice(connectionDetailPrefix.length),
    });
  }
  if (/^Could not connect to this device\. Make sure NodeSend is open on the other device and both devices are on the same network\.?$/i.test(raw)) {
    return i18next.t('transfer.connectionUnreachable', { ns: 'errors' });
  }
  if (/Node identity or protocol version does not match|Protocol version mismatch/i.test(raw)) {
    return i18next.t('transfer.protocolMismatch', { ns: 'errors' });
  }
  if (/QUIC handshake failed:.*(?:NotConnected|UnexpectedEof).*TCP handshake also failed:.*(?:NotConnected|UnexpectedEof)/i.test(raw)) {
    return i18next.t('transfer.protocolHandshakeClosed', { ns: 'errors' });
  }
  if (/Could not connect to the device: it may be offline/i.test(raw)) {
    return i18next.t('transfer.connectionUnreachable', { ns: 'errors' });
  }
  if (/Connection timed out|handshake timed out/i.test(raw)) {
    return i18next.t('transfer.timedOut', { ns: 'errors' });
  }
  if (/insufficient receive-directory space, including reservations from other tasks/i.test(raw)) {
    return i18next.t('transfer.receiveDirectorySpace', { ns: 'errors' });
  }
  if (raw === 'CACHE_SPACE_WAIT_TIMEOUT') {
    return i18next.t('transfer.cacheSpacePaused', { ns: 'errors' });
  }
  if (/PermissionDenied|permission denied|os error 5/i.test(raw)) {
    return i18next.t('generic.permissionDenied', { ns: 'errors' });
  }
  if (/10051|network is unreachable/i.test(raw)) {
    return i18next.t('transfer.connectionUnreachable', { ns: 'errors' });
  }
  if (/10061|connection refused/i.test(raw)) {
    return i18next.t('transfer.connectionRefused', { ns: 'errors' });
  }
  if (/timed? out/i.test(raw)) {
    return i18next.t('transfer.timedOut', { ns: 'errors' });
  }
  const categories: Record<string, string> = {
    DB_ERROR: 'database', IO_ERROR: 'io', CRYPTO_ERROR: 'crypto', CERT_ERROR: 'certificate',
    BAD_REQUEST: 'invalidRequest', NOT_FOUND: 'notFound', FORBIDDEN: 'forbidden',
    CONFLICT: 'conflict', INTERNAL: 'operationFailed',
  };
  const category = categories[code];
  if (category) {
    return i18next.t(`generic.${category}`, { ns: 'errors', reason: raw });
  }
  if (/[\u3400-\u9fff]/.test(raw) && !i18next.language.startsWith('zh-CN')) {
    return i18next.t('generic.unknown', { ns: 'errors' });
  }
  return raw && raw !== '[object Object]'
    ? i18next.t('generic.operationFailed', { ns: 'errors', reason: raw })
    : i18next.t('generic.unknown', { ns: 'errors' });
}
