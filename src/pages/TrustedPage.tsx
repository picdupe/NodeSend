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
import { SidebarToggle, WindowControls } from '../components/AppLayout';
import { useTranslation } from 'react-i18next';
import { errorText } from '../api/network';

export function TrustedPage() {
  const { t, i18n } = useTranslation('common');
  const [nodes, setNodes] = useState<TrustedNode[]>([]);
  const [message, setMessage] = useState('');

  const refresh = useCallback(() => {
    listTrustedNodes().then(setNodes).catch((err) => setMessage(errorText(err)));
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
      setMessage(errorText(err));
    }
  };

  const onRemove = async (node: TrustedNode) => {
    await removeTrustedNode(node.node_id);
    refresh();
  };

  return (
    <div className="page">
      <header className="page-heading">
        <div><span className="eyebrow">NODESEND / TRUSTED</span></div>
        <div className="page-heading-actions">
          <SidebarToggle />
          <WindowControls />
        </div>
      </header>

      <section className="card">
        <h2>{t('trusted.addHeading')}</h2>
        <form
          className="share-form"
          onSubmit={(event) => {
            event.preventDefault();
            onAdd(event.currentTarget);
          }}
        >
          <input name="node_id" placeholder={t('trusted.peerId')} required />
          <input name="display_name" placeholder={t('trusted.displayNameOptional')} />
          <button type="submit">{t('trusted.submit')}</button>
        </form>
      </section>

      <section className="card">
        <h2>{t('trusted.list', { count: nodes.length })}</h2>
        <table className="table">
          <thead>
            <tr>
              <th>Node ID</th>
              <th>{t('trusted.displayName')}</th>
              <th>{t('trusted.addedAt')}</th>
              <th>{t('trusted.actions')}</th>
            </tr>
          </thead>
          <tbody>
            {nodes.map((node) => (
              <tr key={node.node_id}>
                <td className="mono">{node.node_id}</td>
                <td>{node.display_name ?? '-'}</td>
                <td>{new Date(node.created_at).toLocaleString(i18n.language)}</td>
                <td>
                  <button
                    type="button"
                    className="danger"
                    onClick={() => onRemove(node)}
                  >
                    {t('trusted.remove')}
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
