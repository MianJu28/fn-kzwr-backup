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
  export let onPreviewCron = null; // (cron) => Promise<{valid, next, timezone, error}>
  export let onGoto = null; // (pageId) => void
</script>

<!-- 备份路径 + 定时 cron（含未来运行时间预览） -->
<BackupConfigSection
  bind:backupPaths
  bind:targetFolder
  bind:scheduleCron
  bind:scheduleCronValid
  {busy}
  {onSave}
  {onPreviewCron}
/>

<!-- 执行备份 + 结果 -->
<BackupSection {busy} {backupResult} {webdavConfigured} {onRunBackup} {onGoto} />
