<script>
  import UserCard from '../components/UserCard.svelte';
  import WebdavSection from '../components/WebdavSection.svelte';
  import KeySection from '../components/KeySection.svelte';

  export let webdavConfigured = false;
  export let webdavUrl = '';
  export let busy = false;
  export let userInfo = null;
  export let userInfoError = null;
  export let onSaveWebdav = null; // (username, password) => Promise

  // 加密密钥（age）
  export let keyInfo = null; // { public_key }
  export let revealKey = ''; // 首次启动自动生成的私钥（一次性展示）
  export let onSetKey = null; // (privateKey) => Promise
  export let onGenerateKey = null; // () => Promise
</script>

<UserCard {userInfo} {userInfoError} configured={webdavConfigured} />

<WebdavSection configured={webdavConfigured} configuredUrl={webdavUrl} {busy} onSave={onSaveWebdav} />

<KeySection {keyInfo} {revealKey} {busy} {onSetKey} {onGenerateKey} />
