import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// 开发环境后端地址：
// 应用**没有端口直连**，只经飞牛统一网关暴露（/app/fn-kzwr-backup），因此默认代理到
// NAS 上已安装运行的应用网关地址（自签证书 → secure:false）。
// 若要直连 NAS 上手动起的后端：在 NAS 上以 FN_KZWR_DEBUG_PORT=8090 启动后再
// `DEV_API_TARGET=http://192.168.0.105:8090 npm run dev`。
const API_TARGET = process.env.DEV_API_TARGET || 'https://192.168.0.105';

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
    // 开发时代理 API（含 WebSocket /api/ws）到后端：
    // 请求路径形如 /app/fn-kzwr-backup/api/...，网关与后端都按该前缀路由
    proxy: {
      [`${APP_BASE}api`]: {
        target: API_TARGET,
        changeOrigin: true,
        secure: false,
        ws: true,
      },
    },
  },
});
