<script>
  /**
   * 设置页（核心设置）
   *
   * 插件化重构后本页**只保留不属于插件的核心能力**：
   *   加密密钥（age） / 通知（Webhook） / 配置导入导出
   *
   * 已移出本页的内容：
   *   - 插件卡片 → 新的「插件」页（PluginsPage）
   *   - 外置插件（动态库）管理 → 新的「插件」页
   *   - WebDAV 凭据 → 「目标」页（多目标模型，ADR-014）
   *   - kzwr 增强（多账号 token / 空间阈值 / 回收站）→ 「插件」页（由插件 schema 的 accounts 区块渲染）
   *   - 保留策略 → 「任务」页（保留策略是任务级配置，全局那份不生效）
   */
  import KeySection from '../components/KeySection.svelte';
  import NotifySection from '../components/NotifySection.svelte';
  import ConfigSection from '../components/ConfigSection.svelte';

  export let busy = false;

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
</script>

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
