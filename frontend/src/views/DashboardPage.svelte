<script>
  import OverviewSection from '../components/OverviewSection.svelte';
  import UserCard from '../components/UserCard.svelte';
  import SetupCheckSection from '../components/SetupCheckSection.svelte';

  export let backupPaths = [];
  export let targetFolder = 'fn-backup';
  export let webdavConfigured = false;
  export let webdavUrl = '';
  export let restoreFolders = [];
  export let scheduleCron = '';
  export let scheduleCronValid = true;
  export let userInfo = null;
  export let userInfoError = null;
  export let keyBackedUp = false;
  export let kzwr = null; // kzwr 增强信息（存储空间等）
  export let setupResult = null; // 一键体检结果
  export let busy = false;
  export let onSetupCheck = null; // () => Promise
  export let onGoto = null; // (pageId) => void
</script>

<!-- 备份账号（WebDAV） -->
<UserCard {userInfo} {userInfoError} configured={webdavConfigured} {kzwr} />

<!-- 一键体检（配置是否完整、连通性是否正常） -->
<SetupCheckSection result={setupResult} {busy} onCheck={onSetupCheck} {onGoto} />

<!-- 配置概览（实时任务在右侧常驻面板） -->
<OverviewSection
  {backupPaths}
  {targetFolder}
  {webdavConfigured}
  {webdavUrl}
  {restoreFolders}
  {scheduleCron}
  {scheduleCronValid}
  {userInfo}
  {keyBackedUp}
  {onGoto}
/>
