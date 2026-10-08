/**
 * 文件功能：Vite 构建配置。
 * - 仅负责前端 React/TypeScript 的开发服务器与打包；
 * - Tauri 通过 @tauri-apps/cli 在此基础上启动开发模式或加载打包产物（见 src-tauri/tauri.conf.json）；
 * - clearScreen:false 保证 Tauri 的 Rust 编译日志不被 Vite 清屏覆盖；
 * - server.strictPort 固定端口 1420，Tauri 开发模式默认连接该地址。
 */
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  // Only scan the frontend entry; Rust/Android build directories can contain
  // thousands of generated files and are not frontend dependency entries.
  optimizeDeps: { entries: ['index.html'] },
  server: {
    host: '127.0.0.1',
    port: 1420,
    strictPort: true,
    watch: { ignored: ['**/src-tauri/**'] },
  },
});
