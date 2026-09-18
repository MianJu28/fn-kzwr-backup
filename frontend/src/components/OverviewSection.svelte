<script>
  // 当前配置概览（从服务器读取）
  export let backupPaths = [];
  export let targetFolder = 'fn-backup';
  export let webdavConfigured = false;
  export let webdavUrl = '';
  export let restoreFolders = [];
  export let scheduleCron = '';
  export let scheduleCronValid = true;
  export let userInfo = null;

  // WebDAV 目标展示账号用户名；后端取不到账号时回传占位「已配置」，需过滤掉
  $: webdavAccount =
    userInfo && userInfo.username && userInfo.username !== '已配置' ? userInfo.username : '';
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
      <span class="ov-label">WebDAV 目标</span>
      <span class="ov-value">
        {#if webdavConfigured}
          ✅ 已配置{#if webdavAccount}：{webdavAccount}{/if}
          {#if webdavUrl}<span class="ov-sub">{webdavUrl}</span>{/if}
        {:else}
          <span class="muted">⚠️ 未配置</span>
        {/if}
      </span>
    </div>
    <div class="ov-item">
      <span class="ov-label">定时备份</span>
      <span class="ov-value">
        {scheduleCron ? (scheduleCronValid ? `⏰ ${scheduleCron}` : '⚠️ cron 无效') : '未启用'}
      </span>
    </div>
    <div class="ov-item ov-item-wide">
      <span class="ov-label">可恢复 ({restoreFolders.length} 个文件夹)</span>
      <span class="ov-value">
        {#if restoreFolders.length > 0}
          <span class="paths-list">
            {#each restoreFolders as f (f.path)}
              <span class="path-chip">
                {f.path}
                {#if f.files && f.files.length > 0}
                  <span class="file-count">· {f.files.length} 文件</span>
                {/if}
              </span>
            {/each}
          </span>
        {:else}
          <span class="muted">未配置</span>
        {/if}
      </span>
    </div>
  </div>
</section>

<style>
  .overview { background: #f0f7ff; border: 1px solid #cfe4ff; padding: 24px; }
  h2 { margin: 0 0 20px; font-size: 18px; }
  .ov-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 18px;
  }
  @media (max-width: 600px) {
    .ov-grid { grid-template-columns: 1fr; }
  }
  .ov-item {
    background: #fff;
    border: 1px solid #e2efff;
    border-radius: 8px;
    padding: 16px 18px;
  }
  .ov-item-wide {
    grid-column: 1 / -1;
  }
  .ov-label {
    display: block;
    color: #42526e;
    font-weight: 600;
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    margin-bottom: 8px;
  }
  .ov-value { color: #1f2d3d; font-size: 14px; word-break: break-all; line-height: 1.6; }
  .ov-sub { display: block; color: #8a94a6; font-size: 12px; margin-top: 4px; word-break: break-all; }
  .muted { color: #8a94a6; }
  .paths-list { display: flex; flex-wrap: wrap; gap: 6px; }
  .path-chip {
    background: #eef2ff;
    color: #2563eb;
    border-radius: 6px;
    padding: 4px 10px;
    font-family: monospace;
    font-size: 12px;
    word-break: break-all;
  }
  .file-count { color: #8a94a6; font-size: 11px; }
</style>
