export type BrowserProtocol = 'local' | 'nodesend' | 'webdav' | 'smb' | 'ftp';
export interface BrowserConnection { id: string; name: string; protocol: BrowserProtocol; address: string; username: string | null; read_only: boolean; connected: boolean }
export interface BrowserEntry { name: string; path: string; is_dir: boolean; size: number; modified: string | null; can_write: boolean }
export interface BrowserListing { connection_id: string; path: string; entries: BrowserEntry[]; can_write: boolean }
export interface BrowserConnectionInput { name: string; protocol: BrowserProtocol; address: string; username?: string | null; password?: string | null }
