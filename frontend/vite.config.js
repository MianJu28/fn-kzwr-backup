import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// 开发环境后端地址：
// 默认指向飞牛 NAS 上已安装运行的应用（含真实配置）；
// 本地起后端时用 DEV_API_TARGET=http://localhost:8080 覆盖。
const API_TARGET = process.env.DEV_API_TARGET || 'http://192.168.0.105:8080';

// 网关前缀：与 app/ui/config 的 gatewayPrefix、后端 GATEWAY_PREFIX 保持一致。
// 用绝对前缀引用资源，避免「iframe 打开不带斜杠的 /app/<app>/ 时相对路径解析错位」。
const APP_BASE = '/app/fn-kzwr-backup/';

export default defineConfig({
  plugins: [svelte()],
  base: APP_BASE,
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
