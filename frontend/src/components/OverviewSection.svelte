<script>
  // 当前配置概览（从服务器读取）
  export let backupPaths = [];
  export let targetFolder = 'fn-backup';
  export let loggedIn = false;
  export let restoreFolders = [];
  export let scheduleCron = '';
  export let scheduleCronValid = true;
</script>

<section class="overview">
  <h2>📋 当前配置</h2>
  <div class="ov-row">
    <span class="ov-label">备份路径</span>
    <span class="ov-value">{backupPaths.length > 0 ? backupPaths.join('、') : '未配置'}</span>
  </div>
  <div class="ov-row">
    <span class="ov-label">目标文件夹</span>
    <span class="ov-value">{targetFolder || '未配置'}</span>
  </div>
  <div class="ov-row">
    <span class="ov-label">kzwr 登录</span>
    <span class="ov-value">{loggedIn ? '✅ 已登录' : '⚠️ 未登录'}</span>
  </div>
  <div class="ov-row">
    <span class="ov-label">定时备份</span>
    <span class="ov-value">
      {scheduleCron ? (scheduleCronValid ? `⏰ ${scheduleCron}` : '⚠️ cron 无效') : '未启用'}
    </span>
  </div>
  <div class="ov-row">
    <span class="ov-label">可恢复文件</span>
    <span class="ov-value">{restoreFolders.length > 0 ? `${restoreFolders.length} 个文件夹` : '未配置'}</span>
  </div>
</section>

<style>
  .overview { background: #f0f7ff; border: 1px solid #cfe4ff; }
  h2 { margin: 0 0 8px; font-size: 18px; }
  .ov-row {
    display: flex;
    gap: 12px;
    padding: 7px 0;
    font-size: 14px;
    border-bottom: 1px solid #e2efff;
  }
  .ov-row:last-child { border-bottom: none; }
  .ov-label { color: #42526e; font-weight: 600; width: 110px; flex-shrink: 0; }
  .ov-value { color: #1f2d3d; word-break: break-all; }
</style>
