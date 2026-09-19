<script>
  import BackupConfigSection from '../components/BackupConfigSection.svelte';
  import BackupSection from '../components/BackupSection.svelte';

  export let backupPaths = [];
  export let targetFolder = 'fn-backup';
  export let scheduleCron = '';
  export let scheduleCronValid = true;
  export let busy = false;
  export let backupResult = null;
  export let webdavConfigured = false;
  export let onSave = null; // () => Promise
  export let onRunBackup = null; // () => Promise
  export let onGoto = null; // (pageId) => void
</script>

<!-- 备份路径 + 定时 cron -->
<BackupConfigSection
  bind:backupPaths
  bind:targetFolder
  bind:scheduleCron
  bind:scheduleCronValid
  {busy}
  {onSave}
/>

<!-- 执行备份 + 结果 -->
<BackupSection {busy} {backupResult} {webdavConfigured} {onRunBackup} {onGoto} />
