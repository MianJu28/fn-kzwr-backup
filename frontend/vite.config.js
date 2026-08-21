import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  // 构建产物输出到 dist，供 axum 托管
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    // 开发时代理 API 到后端
    proxy: {
      '/api': 'http://localhost:8080',
    },
  },
});
