<script>
  // 配置概览：关键状态统计 + 待办引导
  import Icon from './Icon.svelte';

  export let backupPaths = [];
  export let targetFolder = 'fn-backup';
  export let webdavConfigured = false;
  export let webdavUrl = '';
  export let restoreFolders = [];
  export let scheduleCron = '';
  export let scheduleCronValid = true;
  export let userInfo = null;
  export let keyBackedUp = false;
  export let onGoto = null; // (pageId) => void

  // 后端在取不到账号时回传占位「已配置」，此处过滤
  $: account =
    userInfo && userInfo.username && userInfo.username !== '已配置' ? userInfo.username : '';
  $: fileCount = restoreFolders.reduce((sum, f) => sum + ((f.files || []).length || 0), 0);
  $: cronText = scheduleCron ? scheduleCron : '';
</script>

<div class="stack">
  <section class="card">
    <div class="card-head">
      <div class="icon-wrap"><Icon name="activity" size={18} /></div>
      <div>
        <h2 class="card-title">当前配置</h2>
        <p class="card-desc">备份目标、路径、定时与密钥状态一览</p>
      </div>
    </div>

    <div class="card-body">
      <div class="stat-grid">
        <div class="stat">
          <div class="stat-label"><Icon name="folder" size={13} />备份路径</div>
          <div class="stat-value">{backupPaths.length || '—'}</div>
          <div class="stat-sub">{backupPaths.length ? '个目录' : '尚未配置'}</div>
        </div>

        <div class="stat">
          <div class="stat-label"><Icon name="package" size={13} />目标文件夹</div>
          <div class="stat-value sm">{targetFolder || '—'}</div>
          <div class="stat-sub">云端根目录</div>
        </div>

        <div class="stat">
          <div class="stat-label"><Icon name="cloud" size={13} />WebDAV 目标</div>
          <div class="stat-value sm">
            {#if webdavConfigured}
              <span class="ok-text">已连接</span>
            {:else}
              <span class="warn-text">未配置</span>
            {/if}
          </div>
          <div class="stat-sub">{account || webdavUrl || 'kzwr 官方 WebDAV'}</div>
        </div>

        <div class="stat">
          <div class="stat-label"><Icon name="clock" size={13} />定时备份</div>
          <div class="stat-value sm">
            {#if cronText && scheduleCronValid}
              <span class="mono">{cronText}</span>
            {:else if cronText}
              <span class="warn-text">表达式无效</span>
            {:else}
              未启用
            {/if}
          </div>
          <div class="stat-sub">{cronText ? 'cron（分 时 日 月 周）' : '可手动执行'}</div>
        </div>

        <div class="stat">
          <div class="stat-label"><Icon name="database" size={13} />云端可恢复</div>
          <div class="stat-value">{restoreFolders.length}</div>
          <div class="stat-sub">{fileCount} 个文件快照</div>
        </div>

        <div class="stat">
          <div class="stat-label"><Icon name="key" size={13} />私钥备份</div>
          <div class="stat-value sm">
            {#if keyBackedUp}
              <span class="ok-text">已确认</span>
            {:else}
              <span class="danger-text">待确认</span>
            {/if}
          </div>
          <div class="stat-sub">{keyBackedUp ? '已妥善保存' : '存在丢失风险'}</div>
        </div>
      </div>

      {#if backupPaths.length}
        <div class="paths">
          {#each backupPaths as p (p)}
            <span class="path-chip"><Icon name="folder" size={12} />{p}</span>
          {/each}
        </div>
      {/if}
    </div>

    {#if !webdavConfigured || !backupPaths.length || !keyBackedUp}
      <div class="card-foot guide">
        <span class="guide-label">待完成</span>
        {#if !webdavConfigured}
          <button class="chip" on:click={() => onGoto && onGoto('settings')}>
            <Icon name="cloud" size={13} />配置 WebDAV
          </button>
        {/if}
        {#if !backupPaths.length}
          <button class="chip" on:click={() => onGoto && onGoto('backup')}>
            <Icon name="folder" size={13} />添加备份路径
          </button>
        {/if}
        {#if !keyBackedUp}
          <button class="chip danger" on:click={() => onGoto && onGoto('settings')}>
            <Icon name="key" size={13} />备份私钥
          </button>
        {/if}
      </div>
    {/if}
  </section>
</div>

<style>
  .ok-text {
    color: var(--success);
    font-weight: 600;
  }
  .warn-text {
    color: var(--warn);
    font-weight: 600;
  }
  .danger-text {
    color: var(--danger);
    font-weight: 600;
  }
  .paths {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: var(--s4);
    padding-top: var(--s4);
    border-top: 1px dashed var(--border);
  }
  .guide {
    display: flex;
    align-items: center;
    gap: var(--s2);
    flex-wrap: wrap;
  }
  .guide-label {
    font-size: 11.5px;
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-3);
    margin-right: 2px;
  }
</style>
