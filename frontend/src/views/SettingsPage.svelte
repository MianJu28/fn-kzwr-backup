<script>
  import UserCard from '../components/UserCard.svelte';
  import WebdavSection from '../components/WebdavSection.svelte';
  import RetentionSection from '../components/RetentionSection.svelte';
  import KeySection from '../components/KeySection.svelte';
  import NotifySection from '../components/NotifySection.svelte';
  import ConfigSection from '../components/ConfigSection.svelte';

  export let webdavConfigured = false;
  export let webdavUrl = '';
  export let webdavUsername = '';
  export let busy = false;
  export let userInfo = null;
  export let userInfoError = null;
  export let onSaveWebdav = null; // (username, password) => Promise

  // 保留策略（非敏感配置，回显后可就地修改）
  export let retention = null; // { enabled, cleanup_unmanaged, min_age_days }
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
</script>

<UserCard {userInfo} {userInfoError} configured={webdavConfigured} />

<WebdavSection
  configured={webdavConfigured}
  configuredUrl={webdavUrl}
  configuredUsername={webdavUsername}
  {busy}
  onSave={onSaveWebdav}
/>

<RetentionSection {retention} {busy} onSave={onSaveRetention} />

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
