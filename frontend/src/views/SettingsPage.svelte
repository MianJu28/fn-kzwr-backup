<script>
  /**
   * 设置页
   *
   * 卡片布局：**插件区**由后端 `/api/plugins` 驱动（有哪些卡片、顺序、用内置组件还是
   * 通用 UI Schema 渲染）→ 再加核心区（保留策略 / 加密密钥 / 通知 / 配置导入导出）。
   * 新增插件不必改这个文件，也不必重新打包前端。
   */
  import WebdavSection from '../components/WebdavSection.svelte';
  import KzwrSection from '../components/KzwrSection.svelte';
  import RetentionSection from '../components/RetentionSection.svelte';
  import KeySection from '../components/KeySection.svelte';
  import NotifySection from '../components/NotifySection.svelte';
  import ConfigSection from '../components/ConfigSection.svelte';
  import PluginBlocks from '../components/PluginBlocks.svelte';
  import { sectionsFor, FALLBACK_SECTIONS } from '../lib/plugins.js';

  /** 插件清单（来自 /api/plugins） */
  export let plugins = [];
  /** 插件通用区块操作完成后的回调 */
  export let onPluginDone = null;

  export let webdavConfigured = false;
  export let webdavUrl = '';
  export let webdavUsername = '';
  export let webdavWarning = ''; // 账号一致性提醒
  export let busy = false;
  export let onSaveWebdav = null; // (username, password) => Promise

  // kzwr REST 增强功能（可选）
  export let kzwrConfigured = false;
  export let kzwrUser = null; // { plan, total, used, percentage, error }
  export let kzwrQuotaWarnPercent = 85; // 空间预警阈值（%）
  export let onSaveKzwrToken = null; // (token) => Promise<{success, configured, warning, error}>
  export let onSaveKzwrQuota = null; // (percent) => Promise<{error?}>
  export let onEmptyTrash = null; // () => Promise<{emptied, kept, total_bytes, reason, error}>

  // 保留策略（非敏感配置，回显后可就地修改）
  export let retention = null;
  export let onSaveRetention = null; // (retention) => Promise<{error?}>

  // 加密密钥（age）
  export let keyInfo = null; // { public_key }
  export let revealKey = ''; // 首次启动自动生成的私钥（一次性展示）
  export let keyBackedUp = false; // 用户是否已确认备份私钥
  export let onSetKey = null; // (privateKey) => Promise
  export let onGenerateKey = null; // () => Promise
  export let onExportKey = null; // (passphrase) => Promise<{private_key, error}>
  export let onBackupAck = null; // () => Promise<void>

  // 通知设置（监控告警）
  export let webhookUrl = '';
  export let webhookHeaders = []; // [{ name, value }]
  export let webhookBody = '';
  export let onSaveWebhook = null; // (url, headers, bodyTemplate) => Promise
  export let onTestWebhook = null; // (url, headers, bodyTemplate) => Promise

  // 配置导入/导出
  export let onExportConfig = null; // (passphrase) => Promise<{success, config, error}>
  export let onImportConfig = null; // (passphrase, configText) => Promise<{success, error}>

  // 插件区卡片：顺序与组成由后端决定；接口不可用时退回内置顺序，保证设置页始终可用
  $: pluginSections = sectionsFor(
    plugins && plugins.length ? plugins : FALLBACK_SECTIONS,
    'settings'
  );
</script>

<!-- ── 插件区（由 /api/plugins 驱动）────────────────────────────── -->
{#each pluginSections as p (p.id)}
  {#if p.ui.component === 'webdav'}
    <WebdavSection
      configured={webdavConfigured}
      configuredUrl={webdavUrl}
      configuredUsername={webdavUsername}
      warning={webdavWarning}
      {busy}
      onSave={onSaveWebdav}
    />
  {:else if p.ui.component === 'kzwr'}
    <KzwrSection
      configured={kzwrConfigured}
      user={kzwrUser}
      quotaWarnPercent={kzwrQuotaWarnPercent}
      {busy}
      onSaveToken={onSaveKzwrToken}
      onSaveQuota={onSaveKzwrQuota}
      onEmptyTrash={onEmptyTrash}
    />
  {:else}
    <!-- 前端不认识的内置组件名 → 通用 UI Schema 渲染（外置插件走这条路） -->
    <PluginBlocks plugin={p} onDone={onPluginDone} />
  {/if}
{/each}

<!-- ── 核心区（不属于插件）────────────────────────────────────── -->
<RetentionSection {retention} kzwrReady={kzwrConfigured} {busy} onSave={onSaveRetention} />

<KeySection
  {keyInfo}
  {revealKey}
  backedUp={keyBackedUp}
  {busy}
  {onSetKey}
  {onGenerateKey}
  {onExportKey}
  {onBackupAck}
/>

<NotifySection {webhookUrl} {webhookHeaders} {webhookBody} {busy} onSave={onSaveWebhook} onTest={onTestWebhook} />

<ConfigSection {busy} {onExportConfig} {onImportConfig} />
