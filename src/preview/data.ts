import type { NetworkStatus, TransferTask } from '../types/network';
import type { BrowserConnection, BrowserListing } from '../types/browser';

const remote = 'b'.repeat(64);
const entries = [{ path: '设计稿.pdf', size: 2400000, modified_ms: 0, directory: false }, { path: '封面.png', size: 850000, modified_ms: 0, directory: false }];
const task: TransferTask = { id: 'preview-incoming', peer_id: remote, peer_name: '示例笔记本', direction: 'receive', status: 'waiting', entries, total_bytes: 3250000, completed_bytes: 0, transport: 'quic', error: null, destination: null, events: ['示例接收请求'], created_at: new Date().toISOString(), verify_hash: false };
const state: NetworkStatus = {
  node_id: 'a'.repeat(64), display_name: '界面预览', tcp_port: 58082, quic_port: 58082,
  addresses: ['192.0.2.1'], receive_directory: 'Downloads/NodeSend', receive_policy: 'ask',
  discovery: { mdns: false, udp: false, warnings: [] },
  peers: ['示例笔记本', '示例手机', '示例离线设备'].map((display_name, i) => ({ node_id: [remote, 'c'.repeat(64), 'd'.repeat(64)][i], display_name, endpoints: [{ address: `192.0.2.${i + 2}`, tcp_port: 58082, quic_port: 58082, source: 'preview', last_seen: Date.now() / 1000, error: null }], online: i < 2, trusted: i === 0, verified: true })),
  tasks: [task, { ...task, id: 'preview-history', direction: 'send', status: 'completed', completed_bytes: 3250000, events: ['示例传输记录'] }],
};
const connections: BrowserConnection[] = [{ id: 'preview-files', name: '示例文件', protocol: 'webdav', address: 'https://example.invalid', username: null, read_only: true, connected: true }];

// Memory-only fixtures. Refreshing the page resets all preview changes.
export async function previewInvoke(command: string, args: Record<string, unknown>): Promise<unknown> {
  switch (command) {
    case 'get_network_status': return state;
    case 'get_node_info': return { node_id: state.node_id, http_enabled: false, http_listen: '' };
    case 'list_shares': return [];
    case 'share_protocol_status': return [];
    case 'list_trusted_nodes': return state.peers.filter(p => p.trusted).map(p => ({ node_id: p.node_id, display_name: p.display_name, created_at: task.created_at }));
    case 'choose_send_files': return entries.map(e => e.path);
    case 'preview_transfer': return { entries, chunk_size: 262144, verify_hash: false };
    case 'choose_receive_directory': return 'Downloads/NodeSend';
    case 'set_network_settings':
      state.display_name = String(args.name); state.receive_directory = String(args.directory); state.receive_policy = args.policy as NetworkStatus['receive_policy']; return;
    case 'remove_peer': state.peers = state.peers.filter(p => p.node_id !== args.nodeId); return;
    case 'send_files': {
      const id = `preview-${Date.now()}`;
      state.tasks.unshift({ ...task, id, direction: 'send', peer_id: String(args.peerId), peer_name: state.peers.find(p => p.node_id === args.peerId)?.display_name ?? '示例设备', events: ['界面预览：未实际发送文件'] }); return id;
    }
    case 'decide_incoming_transfer': {
      const item = state.tasks.find(t => t.id === args.id);
      if (item && args.accepted && args.trustDevice) {
        const peer = state.peers.find(p => p.node_id === item.peer_id);
        if (peer) peer.trusted = true;
      }
      if (item) { item.status = args.accepted ? 'accepted' : 'rejected'; item.destination = String(args.destination ?? state.receive_directory); } return;
    }
    case 'list_browser_connections': return connections;
    case 'browse_browser': return { connection_id: String(args.id), path: String(args.path), can_write: false, entries: entries.map(e => ({ name: e.path, path: e.path, is_dir: false, size: e.size, modified: task.created_at, can_write: false })) } satisfies BrowserListing;
    case 'close_browser': { const item = connections.find(c => c.id === args.id); if (item) item.connected = false; return; }
    default: throw new Error('当前为界面预览，此操作需要原生服务；不会连接设备或读写真实文件。');
  }
}
