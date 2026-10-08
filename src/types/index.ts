/**
 * 文件功能：前端 TypeScript 类型定义。
 * - 与 Rust 侧 DTO（ipc::commands / share / registry / httpapi::tree）一一对应；
 * - 字段命名保持与 serde 序列化结果一致，避免运行时映射。
 */

/** 本机 Node 身份与 HTTP 配置（对应 Rust NodeInfo） */
export interface NodeInfo {
  node_id: string;
  http_enabled: boolean;
  http_listen: string;
}

/** 修改 HTTP 配置入参（对应 Rust HttpConfigInput） */
export interface HttpConfigInput {
  enabled: boolean;
  listen?: string;
}

/** 共享访问模式（对应 Rust AccessMode） */
export type AccessMode = 'open' | 'token' | 'trusted' | 'deny';
export type ShareProtocol = 'nodesend' | 'webdav' | 'smb' | 'ftp';
export interface ProtocolStatus { id: string; running: boolean; port: number; error: string | null }

/** 共享目录配置（对应 Rust ShareConfig） */
export interface ShareConfig {
  protocol: ShareProtocol;
  port: number;
  id: string;
  name: string;
  path: string;
  enabled: boolean;
  access_mode: AccessMode;
  access_token: string | null;
  created_at: string;
  updated_at: string;
}

/** 创建共享入参（对应 Rust CreateShareInput） */
export interface CreateShareInput {
  protocol?: ShareProtocol;
  port?: number;
  name: string;
  path: string;
  access_mode?: AccessMode;
}

/** 已信任 Node（对应 Rust registry::TrustedNode） */
export interface TrustedNode {
  node_id: string;
  display_name: string | null;
  created_at: string;
}

/** 目录树节点（对应 Rust httpapi::tree::TreeNode） */
export interface TreeNode {
  name: string;
  path: string;
  node_type: 'dir' | 'file';
  size: number;
  modified: string;
  children?: TreeNode[];
}

/** /list 接口响应 */
export interface ListResponse {
  path: string;
  entries: TreeNode[];
}

