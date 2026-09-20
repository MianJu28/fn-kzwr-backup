<script>
  import OverviewSection from '../components/OverviewSection.svelte';
  import SetupCheckSection from '../components/SetupCheckSection.svelte';

  export let backupPaths = [];
  export let targetFolder = 'fn-backup';
  export let webdavConfigured = false;
  export let webdavUrl = '';
  export let restoreFolders = [];
  export let scheduleCron = '';
  export let scheduleCronValid = true;
  export let keyBackedUp = false;
  export let setupResult = null; // 一键体检结果
  export let busy = false;
  export let onSetupCheck = null; // () => Promise
  export let onGoto = null; // (pageId) => void
</script>

<!-- 一键体检（配置是否完整、连通性是否正常） -->
<SetupCheckSection result={setupResult} {busy} onCheck={onSetupCheck} {onGoto} />

<!-- 配置概览（账号信息在侧边栏常驻） -->
<OverviewSection
  {backupPaths}
  {targetFolder}
  {webdavConfigured}
  {webdavUrl}
  {restoreFolders}
  {scheduleCron}
  {scheduleCronValid}
  {keyBackedUp}
  {onGoto}
/>
