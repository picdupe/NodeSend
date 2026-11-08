/**
 * 文件功能：React 挂载入口。
 * - 将 App 组件挂载到 index.html 的 #root；
 * - 引入全局样式；不包含业务逻辑。
 */
import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import './styles/global.css';
import { initializeI18n } from './i18n';
import { initializeAccent } from './theme';
import { initializeAppearance } from './appearance';

document.addEventListener('contextmenu', (event) => event.preventDefault());
initializeAppearance();

void initializeI18n().finally(() => {
  initializeAccent();
  ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
});
