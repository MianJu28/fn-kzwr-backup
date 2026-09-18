<script>
  import UserCard from '../components/UserCard.svelte';
  import WebdavSection from '../components/WebdavSection.svelte';
  import KeySection from '../components/KeySection.svelte';
  import NotifySection from '../components/NotifySection.svelte';

  export let webdavConfigured = false;
  export let webdavUrl = '';
  export let busy = false;
  export let userInfo = null;
  export let userInfoError = null;
  export let onSaveWebdav = null; // (username, password) => Promise

  // 加密密钥（age）
  export let keyInfo = null; // { public_key }
  export let revealKey = ''; // 首次启动自动生成的私钥（一次性展示）
  export let keyBackedUp = false; // 用户是否已确认备份私钥
  export let onSetKey = null; // (privateKey) => Promise
  export let onGenerateKey = null; // () => Promise
  export let onExportKey = null; // () => Promise<{private_key, error}>
  export let onBackupAck = null; // () => Promise<void>

  // 通知设置（监控告警）
  export let webhookUrl = '';
  export let onSaveWebhook = null; // (url) => Promise
</script>

<UserCard {userInfo} {userInfoError} configured={webdavConfigured} />

<WebdavSection configured={webdavConfigured} configuredUrl={webdavUrl} {busy} onSave={onSaveWebdav} />

<KeySection
  {keyInfo}
  {revealKey}
  {keyBackedUp}
  {busy}
  {onSetKey}
  {onGenerateKey}
  {onExportKey}
  {onBackupAck}
/>

<NotifySection {webhookUrl} {busy} onSave={onSaveWebhook} />
