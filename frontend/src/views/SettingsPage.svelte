<script>
  import WebdavSection from '../components/WebdavSection.svelte';
  import KzwrSection from '../components/KzwrSection.svelte';
  import RetentionSection from '../components/RetentionSection.svelte';
  import KeySection from '../components/KeySection.svelte';
  import NotifySection from '../components/NotifySection.svelte';
  import ConfigSection from '../components/ConfigSection.svelte';
  import Icon from '../components/Icon.svelte';

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

  // 调试日志（开发者选项）
  export let debug = false;
  export let onSaveDebug = null; // (enabled) => Promise<{error?}>
</script>

<WebdavSection
  configured={webdavConfigured}
  configuredUrl={webdavUrl}
  configuredUsername={webdavUsername}
  warning={webdavWarning}
  {busy}
  onSave={onSaveWebdav}
/>

<KzwrSection
  configured={kzwrConfigured}
  user={kzwrUser}
  quotaWarnPercent={kzwrQuotaWarnPercent}
  {busy}
  onSaveToken={onSaveKzwrToken}
  onSaveQuota={onSaveKzwrQuota}
  onEmptyTrash={onEmptyTrash}
/>

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

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="info" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">开发者选项</h2>
      <p class="card-desc">调试日志：记录网络请求/响应明细与关键流程细节，便于问题定位</p>
    </div>
  </div>
  <div class="card-body">
    <label class="dbg-row">
      <input
        type="checkbox"
        checked={debug}
        on:change={(e) => onSaveDebug && onSaveDebug(e.currentTarget.checked)}
        disabled={busy || !onSaveDebug}
      />
      <span class="dbg-text">
        启用调试日志
        <small>切换后立即生效并持久化；日志可见于服务端输出（journalctl / 容器日志）</small>
      </span>
    </label>
  </div>
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .dbg-row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    cursor: pointer;
  }
  .dbg-row input {
    margin-top: 2px;
    accent-color: var(--primary);
  }
  .dbg-text {
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 13px;
    color: var(--text);
  }
  .dbg-text small {
    color: var(--text-3);
    font-size: 12px;
  }
</style>
