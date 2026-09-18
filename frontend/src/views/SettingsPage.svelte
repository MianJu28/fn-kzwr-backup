<script>
  import UserCard from '../components/UserCard.svelte';
  import WebdavSection from '../components/WebdavSection.svelte';

  export let webdavConfigured = false;
  export let webdavUrl = '';
  export let busy = false;
  export let userInfo = null;
  export let userInfoError = null;
  export let onSaveWebdav = null; // (url, username, password) => Promise
</script>

<UserCard {userInfo} {userInfoError} {webdavConfigured} />

<WebdavSection configured={webdavConfigured} configuredUrl={webdavUrl} {busy} onSave={onSaveWebdav} />
