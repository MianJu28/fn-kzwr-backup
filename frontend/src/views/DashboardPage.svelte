<script>
  import LiveStatus from '../components/LiveStatus.svelte';
  import OverviewSection from '../components/OverviewSection.svelte';
  import UserCard from '../components/UserCard.svelte';

  export let liveStatus = null;
  export let backupPaths = [];
  export let targetFolder = 'fn-backup';
  export let loggedIn = false;
  export let restoreFolders = [];
  export let scheduleCron = '';
  export let scheduleCronValid = true;
  export let userInfo = null;
  export let userInfoError = null;
</script>

<!-- 登录用户 + 存储容量 -->
<UserCard {userInfo} {userInfoError} />

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
