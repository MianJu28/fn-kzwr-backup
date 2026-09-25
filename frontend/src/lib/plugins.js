/**
 * 前端插件注册表
 *
 * 页面上「有哪些插件卡片、什么顺序、用哪个组件渲染」**全部来自后端 `/api/plugins`**，
 * 前端不硬编码具体插件（新增插件不必改这里、也不必重新打包前端）：
 * - 后端返回 `ui.component`（如 `webdav` / `kzwr`）→ 用内置组件渲染（体验最好）
 * - 不认识该组件 → 回退到 `ui.blocks` 的通用渲染（components/PluginBlocks.svelte）
 *
 * 本模块只负责：拉取、缓存、按分区过滤排序。
 */
import { api } from './api.js';

/** 缓存：PluginEntry[] */
let cache = null;
let inflight = null;

/** 拉取插件清单（默认走缓存；`force` 强制刷新，例如配置变化后） */
export async function loadPlugins(force = false) {
  if (cache && !force) return cache;
  if (inflight) return inflight;
  inflight = api
    .plugins()
    .then((d) => {
      cache = d.plugins || [];
      return cache;
    })
    .catch(() => {
      // 拉取失败不阻塞页面：返回空表，由调用方决定兜底
      cache = cache || [];
      return cache;
    })
    .finally(() => {
      inflight = null;
    });
  return inflight;
}

/** 清空缓存（退出登录/切换后端时用） */
export function clearPlugins() {
  cache = null;
}

/**
 * 某分区的区块（已按 `ui.order` 排序）：`settings` | `dashboard`
 *
 * 说明：**不按 `available` 过滤** —— 未配置的插件也要露出卡片，用户才能去配置它；
 * `available` 留给组件内部决定要不要显示「未启用」徽标或提示。
 */
export function sectionsFor(plugins, section = 'settings') {
  return (plugins || [])
    .filter((p) => p && p.ui && p.ui.section === section)
    .sort((a, b) => (a.ui.order || 0) - (b.ui.order || 0));
}

/** 前端**已内置**的组件名（其余插件走通用 UI Schema 渲染） */
export const BUILTIN_COMPONENTS = ['webdav', 'kzwr'];

/**
 * `/api/plugins` 不可用时的兜底区块（保持页面可用，不依赖网络）
 *
 * 仅用于展示位置与顺序；正常路径永远以接口返回为准。
 */
export const FALLBACK_SECTIONS = [
  {
    id: 'webdav',
    name: 'WebDAV',
    kind: 'target',
    builtin: true,
    available: true,
    api_base: '',
    ui: { section: 'settings', title: '备份目标（WebDAV）', order: 10, component: 'webdav', blocks: [] },
  },
  {
    id: 'kzwr',
    name: '酷族账号增强',
    kind: 'enhance',
    builtin: true,
    available: true,
    api_base: '/api/p/kzwr',
    ui: { section: 'settings', title: '增强功能（酷族账号）', order: 20, component: 'kzwr', blocks: [] },
  },
];
