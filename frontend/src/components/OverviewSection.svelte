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
  <div class="ov-grid">
    <div class="ov-item">
      <span class="ov-label">备份路径</span>
      <span class="ov-value">
        {#if backupPaths.length > 0}
          <span class="paths-list">
            {#each backupPaths as p (p)}
              <span class="path-chip">{p}</span>
            {/each}
          </span>
        {:else}
          <span class="muted">未配置</span>
        {/if}
      </span>
    </div>
    <div class="ov-item">
      <span class="ov-label">目标文件夹</span>
      <span class="ov-value">
        {#if targetFolder}
          {targetFolder}
        {:else}
          <span class="muted">未配置</span>
        {/if}
      </span>
    </div>
    <div class="ov-item">
      <span class="ov-label">kzwr 登录</span>
      <span class="ov-value">{loggedIn ? '✅ 已登录' : '⚠️ 未登录'}</span>
    </div>
    <div class="ov-item">
      <span class="ov-label">定时备份</span>
      <span class="ov-value">
        {scheduleCron ? (scheduleCronValid ? `⏰ ${scheduleCron}` : '⚠️ cron 无效') : '未启用'}
      </span>
    </div>
    <div class="ov-item">
      <span class="ov-label">可恢复</span>
      <span class="ov-value">{restoreFolders.length > 0 ? `${restoreFolders.length} 个文件夹` : '未配置'}</span>
    </div>
  </div>
</section>

<style>
  .overview { background: #f0f7ff; border: 1px solid #cfe4ff; }
  h2 { margin: 0 0 14px; font-size: 18px; }
  .ov-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
    gap: 14px;
  }
  .ov-item {
    background: #fff;
    border: 1px solid #e2efff;
    border-radius: 8px;
    padding: 12px 14px;
  }
  .ov-label {
    display: block;
    color: #42526e;
    font-weight: 600;
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    margin-bottom: 6px;
  }
  .ov-value { color: #1f2d3d; font-size: 14px; word-break: break-all; line-height: 1.5; }
  .muted { color: #8a94a6; }
  .paths-list { display: flex; flex-wrap: wrap; gap: 6px; }
  .path-chip {
    background: #eef2ff;
    color: #2563eb;
    border-radius: 6px;
    padding: 3px 8px;
    font-family: monospace;
    font-size: 12px;
    word-break: break-all;
  }
</style>
