import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// 开发环境后端地址：
// 默认指向飞牛 NAS 上已安装运行的应用（含真实配置）；
// 本地起后端时用 DEV_API_TARGET=http://localhost:8080 覆盖。
const API_TARGET = process.env.DEV_API_TARGET || 'http://192.168.0.105:8080';

export default defineConfig({
  plugins: [svelte()],
  // 构建产物输出到 dist，供 axum 托管
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    host: true,
    port: 5173,
    // 开发时代理 API（含 WebSocket /api/ws）到后端
    proxy: {
      '/api': {
        target: API_TARGET,
        changeOrigin: true,
        ws: true,
      },
    },
  },
});
