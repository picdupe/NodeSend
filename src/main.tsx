/**
 * 文件功能：React 挂载入口。
 * - 将 App 组件挂载到 index.html 的 #root；
 * - 引入全局样式；不包含业务逻辑。
 */
import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import './styles/global.css';

// Keep the desktop surface consistent with native app behavior.
document.addEventListener('contextmenu', (event) => event.preventDefault());

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
