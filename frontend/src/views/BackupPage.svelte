<script>
  import BackupConfigSection from '../components/BackupConfigSection.svelte';
  import BackupSection from '../components/BackupSection.svelte';

  export let backupPaths = [];
  export let targetFolder = 'fn-backup';
  export let scheduleCron = '';
  export let scheduleCronValid = true;
  export let busy = false;
  export let backupResult = null;
  export let onSave = null; // () => Promise
  export let onRunBackup = null; // () => Promise
</script>

<!-- 备份路径配置 + 定时 cron -->
<BackupConfigSection
  bind:backupPaths
  bind:targetFolder
  bind:scheduleCron
  bind:scheduleCronValid
  {busy}
  {onSave}
/>

<!-- 备份 -->
<BackupSection {busy} {backupResult} {onRunBackup} />
