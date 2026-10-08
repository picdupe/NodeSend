/**
 * 文件功能：目录树渲染组件（展示 HTTP 目录树 API 返回的 TreeNode）。
 *
 * - 递归渲染，目录可折叠展开，文件显示大小与修改时间；
 * - key 规范：类型前缀 + 完整虚拟路径（dir:/a/b、file:/a/c），
 *   保证同名节点唯一、父子挂载稳定（经验教训）；
 * - 目录默认折叠；根节点默认展开；
 * - 不做数据请求（数据由父页面通过 httpShare.fetchTree 取得并传入）。
 */
import { useState } from 'react';
import type { TreeNode } from '../types';

interface DirectoryTreeProps {
  node: TreeNode;
  /** 根节点是否默认展开 */
  defaultExpanded?: boolean;
}

export function DirectoryTree({ node, defaultExpanded = true }: DirectoryTreeProps) {
  return (
    <ul className="tree">
      <TreeItem node={node} defaultExpanded={defaultExpanded} depth={0} />
    </ul>
  );
}

interface TreeItemProps {
  node: TreeNode;
  defaultExpanded: boolean;
  depth: number;
}

function TreeItem({ node, defaultExpanded, depth }: TreeItemProps) {
  const isDir = node.node_type === 'dir';
  const [expanded, setExpanded] = useState(defaultExpanded);
  const nodeKey = `${isDir ? 'dir' : 'file'}:${node.path || '/'}`;

  return (
    <li key={nodeKey}>
      <div
        className={`tree-row ${isDir ? 'tree-dir' : 'tree-file'}`}
        style={{ paddingLeft: depth * 18 }}
        onClick={() => isDir && setExpanded((value) => !value)}
      >
        <span className="tree-icon">{isDir ? (expanded ? '▾' : '▸') : '•'}</span>
        <span className="tree-name" title={node.path || '/'}>
          {node.name}
        </span>
        {!isDir && (
          <span className="tree-meta">
            {formatSize(node.size)} · {formatDate(node.modified)}
          </span>
        )}
      </div>
      {isDir && expanded && node.children && node.children.length > 0 && (
        <ul>
          {node.children.map((child) => (
            <TreeItem
              key={`${child.node_type}:${child.path || '/'}`}
              node={child}
              defaultExpanded={false}
              depth={depth + 1}
            />
          ))}
        </ul>
      )}
    </li>
  );
}

function formatSize(size: number): string {
  if (size < 1024) return `${size.toFixed(2)} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = size / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(2)} ${units[unit]}`;
}

function formatDate(value: string): string {
  if (!value) return '';
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? value
    : date.toLocaleString('zh-CN', { hour12: false });
}
