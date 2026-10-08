export interface Endpoint { address: string; tcp_port: number; quic_port: number; source: string; last_seen: number; error: string | null }
export interface Peer { node_id: string; display_name: string; endpoints: Endpoint[]; online: boolean; trusted: boolean; verified: boolean }
export interface FileEntry { path: string; size: number; modified_ms: number; directory: boolean }
export interface Manifest { entries: FileEntry[]; chunk_size: number; verify_hash: boolean }
export type TaskStatus = 'queued' | 'connecting' | 'waiting' | 'accepted' | 'transferring' | 'paused' | 'interrupted' | 'failed' | 'completed' | 'rejected' | 'cancelled';
export interface TransferTask { warnings?: string[]; bytes_per_second?: number; id: string; peer_id: string; peer_name: string; direction: 'send' | 'receive'; status: TaskStatus; entries: FileEntry[]; total_bytes: number; completed_bytes: number; transport: 'quic' | 'tcp' | null; error: string | null; destination: string | null; events: string[]; created_at: string; verify_hash: boolean }
export interface NetworkStatus { node_id: string; display_name: string; tcp_port: number; quic_port: number; addresses: string[]; discovery: { mdns: boolean; udp: boolean; warnings: string[] }; peers: Peer[]; tasks: TransferTask[]; receive_directory: string; receive_policy: 'ask' | 'trusted' | 'reject' }
export type ConflictPolicy = 'rename' | 'skip' | 'overwrite';


