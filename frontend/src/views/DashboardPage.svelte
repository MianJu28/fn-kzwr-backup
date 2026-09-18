<script>
  import OverviewSection from '../components/OverviewSection.svelte';
  import UserCard from '../components/UserCard.svelte';

  export let backupPaths = [];
  export let targetFolder = 'fn-backup';
  export let webdavConfigured = false;
  export let webdavUrl = '';
  export let restoreFolders = [];
  export let scheduleCron = '';
  export let scheduleCronValid = true;
  export let userInfo = null;
  export let userInfoError = null;
</script>

<!-- 备份账号（WebDAV） -->
<UserCard {userInfo} {userInfoError} configured={webdavConfigured} />

<!-- 当前配置概览（实时任务已移到右侧常驻面板） -->
<OverviewSection
  {backupPaths}
  {targetFolder}
  {webdavConfigured}
  {webdavUrl}
  {restoreFolders}
  {scheduleCron}
  {scheduleCronValid}
  {userInfo}
/>
