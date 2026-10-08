/**
 * 文件功能：Tauri IPC 调用封装层。
 * - 所有对 Rust Service 的调用集中在此（基线 §46：UI 通过本机 IPC 控制 Service）；
 * - React 页面只引用本文件，不直接写 invoke(...)，便于接口变更时统一修改；
 * - 浏览器开发环境使用内存示例数据，原生环境调用真实 IPC。
 */
import { invoke } from './runtime';
import type {
  CreateShareInput,
  HttpConfigInput,
  NodeInfo,
  ShareConfig,
  TrustedNode,
} from '../types';

/** 读取本机 Node 身份与 HTTP 服务状态 */
export function getNodeInfo(): Promise<NodeInfo> {
  return invoke<NodeInfo>('get_node_info');
}
export function startShareLink(): Promise<number> {
  return invoke<number>('start_share_link');
}

/** 修改 HTTP 服务配置 */
export function setHttpConfig(config: HttpConfigInput): Promise<void> {
  return invoke('set_http_config', { config });
}

/** 列出全部共享目录配置 */
export function listShares(): Promise<ShareConfig[]> {
  return invoke<ShareConfig[]>('list_shares');
}
export function shareProtocolStatus() {
  return invoke<import('../types').ProtocolStatus[]>('share_protocol_status');
}

/** 创建共享 */
export function createShare(input: CreateShareInput): Promise<ShareConfig> {
  return invoke<ShareConfig>('create_share', { input });
}

/** 启用/禁用共享 */
export function toggleShare(id: string, enabled: boolean): Promise<ShareConfig> {
  return invoke<ShareConfig>('toggle_share', { id, enabled });
}

/** 轮换共享访问令牌 */
export function rotateShareToken(id: string): Promise<ShareConfig> {
  return invoke<ShareConfig>('rotate_share_token', { id });
}

/** 删除共享配置 */
export function deleteShare(id: string): Promise<void> {
  return invoke('delete_share', { id });
}

/** 列出已信任 Node */
export function listTrustedNodes(): Promise<TrustedNode[]> {
  return invoke<TrustedNode[]>('list_trusted_nodes');
}

/** 添加信任 Node */
export function addTrustedNode(
  nodeId: string,
  displayName?: string,
): Promise<void> {
  return invoke('add_trusted_node', {
    nodeId,
    displayName: displayName ?? null,
  });
}

/** 移除信任 Node */
export function removeTrustedNode(nodeId: string): Promise<void> {
  return invoke('remove_trusted_node', { nodeId });
}
