/**
 * 文件功能：信任管理页面（基线 §3 Trust、§16）。
 *
 * - 展示 trusted_nodes 列表；
 * - 手动添加信任 Node（输入 Node ID 与显示名称）；
 * - 移除信任（移除后再次发现同 ID 需重新 Trust）。
 */
import { useCallback, useEffect, useState } from 'react';
import {
  addTrustedNode,
  listTrustedNodes,
  removeTrustedNode,
} from '../api/ipc';
import type { TrustedNode } from '../types';
import { SidebarToggle } from '../components/AppLayout';

export function TrustedPage() {
  const [nodes, setNodes] = useState<TrustedNode[]>([]);
  const [message, setMessage] = useState('');

  const refresh = useCallback(() => {
    listTrustedNodes().then(setNodes).catch((err) => setMessage(String(err)));
  }, []);
  useEffect(refresh, [refresh]);

  const onAdd = async (form: HTMLFormElement) => {
    const data = new FormData(form);
    setMessage('');
    try {
      await addTrustedNode(
        String(data.get('node_id') ?? ''),
        String(data.get('display_name') ?? '') || undefined,
      );
      form.reset();
      refresh();
    } catch (err) {
      setMessage(String(err));
    }
  };

  const onRemove = async (node: TrustedNode) => {
    await removeTrustedNode(node.node_id);
    refresh();
  };

  return (
    <div className="page">
      <header className="page-heading">
        <h1>信任管理</h1>
        <div className="page-heading-actions">
          <SidebarToggle />
        </div>
      </header>

      <section className="card">
        <h2>添加信任 Node</h2>
        <form
          className="share-form"
          onSubmit={(event) => {
            event.preventDefault();
            onAdd(event.currentTarget);
          }}
        >
          <input name="node_id" placeholder="对端 Node ID" required />
          <input name="display_name" placeholder="显示名称（可选）" />
          <button type="submit">添加</button>
        </form>
      </section>

      <section className="card">
        <h2>已信任 Node（{nodes.length}）</h2>
        <table className="table">
          <thead>
            <tr>
              <th>Node ID</th>
              <th>显示名称</th>
              <th>添加时间</th>
              <th>操作</th>
            </tr>
          </thead>
          <tbody>
            {nodes.map((node) => (
              <tr key={node.node_id}>
                <td className="mono">{node.node_id}</td>
                <td>{node.display_name ?? '-'}</td>
                <td>{new Date(node.created_at).toLocaleString('zh-CN')}</td>
                <td>
                  <button
                    type="button"
                    className="danger"
                    onClick={() => onRemove(node)}
                  >
                    移除
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {message && <p className="hint">{message}</p>}
      </section>
    </div>
  );
}
