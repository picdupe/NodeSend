import { invoke } from './runtime';
import type { ConflictPolicy, Manifest, NetworkStatus, Peer } from '../types/network';
export const getNetworkStatus = () => invoke<NetworkStatus>('get_network_status');
export const chooseSendFiles = (directory = false, kind: 'files' | 'media' | 'apps' = 'files') => invoke<string[]>('choose_send_files', { directory, kind });
export const takeSharedFiles = () => invoke<string[]>('take_shared_files');
export const stageTextPayload = (text: string) => invoke<string>('stage_text_payload', { text });
export const stageBinaryPayload = (bytes: number[], extension: string) => invoke<string>('stage_binary_payload', { bytes, extension });
export const readClipboardFiles = () => invoke<string[]>('read_clipboard_files');
export const chooseReceiveDirectory = () => invoke<string | null>('choose_receive_directory');
export const previewTransfer = (paths: string[]) => invoke<Manifest>('preview_transfer', { paths });
export const sendFiles = (peerId: string, paths: string[], verifyHash: boolean, allowPublic: boolean) => invoke<string>('send_files', { peerId, paths, verifyHash, allowPublic });
export const cancelSendPreflight = (peerId: string) => invoke<void>('cancel_send_preflight', { peerId });
export const addEndpoint = (address: string, expectedNodeId: string | null, allowPublic: boolean) => invoke<Peer>('add_endpoint', { address, expectedNodeId, allowPublic });
export const removePeer = (nodeId: string) => invoke<void>('remove_peer', { nodeId });
export const decideIncoming = (id: string, accepted: boolean, destination: string | null, conflict: ConflictPolicy, selected: string[] | null = null, trustDevice = false) => invoke<void>('decide_incoming_transfer', { id, accepted, destination, conflict, selected, trustDevice });
export const transferAction = (id: string, action: string) => invoke<void>('transfer_action', { id, action });
export const setNetworkSettings = (name: string, directory: string, policy: string) => invoke<void>('set_network_settings', { name, directory, policy });
export function errorText(error: unknown): string {
  const raw = typeof error === 'object' && error && 'message' in error ? String(error.message) : String(error);
  if (/10051|无法连接的网络|network is unreachable/i.test(raw)) {
    return '暂时无法连接这台设备。请确认对方已打开 NodeSend，并且两台设备连接到同一网络。';
  }
  if (/10061|connection refused|拒绝连接/i.test(raw)) {
    return '对方设备没有接受连接。请确认 NodeSend 正在运行，并检查防火墙设置。';
  }
  if (/timed? out|超时/i.test(raw)) {
    return '连接设备超时。请确认对方在线，并检查网络和防火墙。';
  }
  return raw;
}
