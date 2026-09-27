<script>
  /**
   * 插件页
   *
   * 插件化后的**一等公民页面**：这里集中展示与管理全部插件能力。
   *
   * 结构（全部由后端 `/api/plugins` 驱动，新增插件无需改前端、无需重新打包）：
   *   1. 插件卡片区 —— 按 `ui.order` 排序，用通用 UI Schema 渲染（`PluginBlocks`）
   *   2. 外置插件管理 —— 开关 / 目录 / 签名公钥 / 诊断 / 卸载（`PluginSection`）
   *
   * 设计要点：后端 `component` 字段已弃用（内置插件也改为 schema 描述），
   * 因此这里**不再**做「内置组件 vs 通用渲染」的分支，插件一律 schema 渲染。
   */
  import PluginBlocks from '../components/PluginBlocks.svelte';
  import PluginSection from '../components/PluginSection.svelte';
  import { sectionsFor, FALLBACK_SECTIONS } from '../lib/plugins.js';

  /** 插件清单（来自 /api/plugins） */
  export let plugins = [];
  /** 插件通用区块操作完成后的回调（如 token 保存后刷新账号信息） */
  export let onPluginDone = null;

  // 外置插件加载（动态库，ADR-013 方案 B）
  export let pluginsEnabled = false;
  export let pluginsDir = '';
  /** 插件签名公钥（base64 32 字节 Ed25519 公钥，每行一个） */
  export let pluginsPubkeys = [];
  /** 是否由环境变量放行未签名插件（只读，仅本机调试） */
  export let pluginsAllowUnsigned = false;
  export let onSavePlugins = null; // (enabled, dir, pubkeys) => Promise<{error?}>

  export let busy = false;

  // 插件卡片：顺序与组成由后端决定；接口不可用时退回内置兜底，保证页面始终可用
  $: pluginSections = sectionsFor(
    plugins && plugins.length ? plugins : FALLBACK_SECTIONS,
    'settings'
  );
</script>

<!-- ── 插件卡片区（全部走通用 UI Schema）──────────────────────── -->
{#each pluginSections as p (p.id)}
  <PluginBlocks plugin={p} onDone={onPluginDone} />
{/each}

<!-- ── 外置插件（动态库）管理 ─────────────────────────────────── -->
<PluginSection
  enabled={pluginsEnabled}
  dir={pluginsDir}
  pubkeys={pluginsPubkeys}
  allowUnsigned={pluginsAllowUnsigned}
  {busy}
  onSave={onSavePlugins}
/>
