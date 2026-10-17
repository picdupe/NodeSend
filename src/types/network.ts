export interface Endpoint { address: string; tcp_port: number; quic_port: number; source: string; last_seen: number; error: string | null }
export interface Peer { node_id: string; display_name: string; endpoints: Endpoint[]; online: boolean; trusted: boolean; verified: boolean }
export interface FileEntry { path: string; size: number; modified_ms: number; directory: boolean }
/** `list_task_entries` 的分页响应：明细按页加载，避免百万级清单一次性传入前端。 */
export interface TaskEntriesPage { entries: FileEntry[]; total: number }
/** `preview_transfer` 只返回计数与总量，避免把整份清单经 IPC 传输到前端。 */
export interface ManifestSummary { scan_complete: boolean; top_level_count: number; file_count: number; directory_count: number; total_size: number; chunk_count: number; chunk_size: number; verify_hash: boolean }
/** 发送前预览：后台持续扫描整个选择；`scanning` 为 false 时计数即最终值（此时界面直接显示结果）。`exceeded` 只在扫描过程中用于提示"文件数量过多"，扫描不会因此停止。 */
export interface PreviewStatus { scanning: boolean; exceeded: boolean; top_level_count: number; file_count: number; directory_count: number; total_size: number; chunk_count: number; chunk_size: number }
export type TaskStatus = 'queued' | 'connecting' | 'waiting' | 'accepted' | 'transferring' | 'paused' | 'remote_paused' | 'interrupted' | 'failed' | 'incompatible' | 'completed' | 'rejected' | 'cancelling' | 'cancelled';
export interface TransferTask { warnings?: string[]; bytes_per_second?: number; id: string; peer_id: string; peer_name: string; peer_device_type?: string; direction: 'send' | 'receive'; status: TaskStatus; scan_complete: boolean; top_level_count: number; file_count: number; directory_count: number; total_bytes: number; completed_bytes: number; transport: 'quic' | 'tcp' | null; error: string | null; destination: string | null; events: string[]; created_at: string; verify_hash: boolean }
export interface NetworkStatus { node_id: string; display_name: string; tcp_port: number; quic_port: number; addresses: string[]; discovery: { mdns: boolean; udp: boolean; warnings: string[] }; peers: Peer[]; tasks: TransferTask[]; receive_directory: string; cache_directory: string; receive_policy: 'ask' | 'trusted' | 'reject' }
export type ConflictPolicy = 'rename' | 'skip' | 'overwrite';


