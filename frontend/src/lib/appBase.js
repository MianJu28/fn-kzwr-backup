/**
 * 应用挂载前缀（必须与三处保持一致）
 *
 * 1. `packaging/fn-kzwr-backup-app/app/ui/config` 的 `gatewayPrefix`
 * 2. 后端 `main.rs` 的 `gateway_prefix()`（可用 GATEWAY_PREFIX 覆盖，默认同值）
 * 3. 本文件 + `vite.config.js` 的 `base`
 *
 * 背景：飞牛桌面可能以 https 访问，而应用若只提供 http 端口，iframe 会因
 * **混合内容**被浏览器拦截。飞牛「统一网关」把应用反代到 `/app/<appname>`
 * 下（与桌面同源同协议，支持 WebSocket），因此前端资源用该绝对前缀引用，
 * API 与 WebSocket 也必须带上前缀——否则请求会落到宿主的 `/api` 上。
 *
 * 开发模式（vite dev server）下前缀为空：`/api` 由 vite 代理到后端。
 */
export const APP_BASE = import.meta.env.DEV ? '' : '/app/fn-kzwr-backup';
