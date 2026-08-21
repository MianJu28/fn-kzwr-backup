<script>
  import LiveStatus from '../components/LiveStatus.svelte';
  import OverviewSection from '../components/OverviewSection.svelte';

  export let liveStatus = null;
  export let backupPaths = [];
  export let targetFolder = 'fn-backup';
  export let loggedIn = false;
  export let restoreFolders = [];
  export let scheduleCron = '';
  export let scheduleCronValid = true;
</script>

<!-- 实时任务状态（WebSocket 推送） -->
<LiveStatus {liveStatus} />

<!-- 当前配置概览 -->
<OverviewSection
  {backupPaths}
  {targetFolder}
  {loggedIn}
  {restoreFolders}
  {scheduleCron}
  {scheduleCronValid}
/>
