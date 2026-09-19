<script>
  // 立即备份执行入口 + 本次结果统计
  import Icon from './Icon.svelte';
  import { fmtBytes } from '../lib/format.js';

  export let busy = false;
  export let backupResult = null;
  export let webdavConfigured = true;
  export let onRunBackup = null; // () => Promise
  export let onGoto = null; // (pageId) => void

  $: ok = backupResult && !backupResult.error;
  $: skipped = !!(backupResult && backupResult.skipped);
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="upload" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">执行备份</h2>
      <p class="card-desc">
        将配置的文件夹增量加密后上传至 kzwr 云盘；已加密的文件不会重复上传。
      </p>
    </div>
  </div>

  <div class="card-body">
    {#if !webdavConfigured}
      <div class="alert alert-warn">
        <Icon name="alert" size={16} />
        <div class="alert-body">
          尚未配置 WebDAV 凭据，无法执行备份。
          <button class="btn btn-sm btn-soft inline" on:click={() => onGoto && onGoto('settings')}>
            前往设置<Icon name="arrow-right" size={13} />
          </button>
        </div>
      </div>
    {/if}

    <button
      class="btn btn-primary btn-lg btn-block"
      on:click={onRunBackup}
      disabled={busy || !webdavConfigured}
    >
      {#if busy}
        <span class="spin"></span>备份执行中…
      {:else}
        <Icon name="zap" size={16} />立即备份
      {/if}
    </button>
    <p class="field-hint center">执行期间可在右侧「实时任务」面板查看进度</p>
  </div>

  {#if backupResult}
    <div class="card-foot">
      {#if skipped}
        <div class="alert alert-warn">
          <Icon name="clock" size={16} />
          <div class="alert-body">
            <div class="alert-title">本次已跳过</div>
            {backupResult.error || '已有备份任务在执行中'}
          </div>
        </div>
      {:else if ok}
        <div class="result">
          <div class="result-head">
            <Icon name="check-circle" size={17} />
            <span>备份完成</span>
          </div>
          <div class="stat-grid">
            <div class="stat">
              <div class="stat-label"><Icon name="upload" size={12} />上传</div>
              <div class="stat-value">{backupResult.uploaded}</div>
              <div class="stat-sub">{fmtBytes(backupResult.uploaded_bytes)}</div>
            </div>
            <div class="stat">
              <div class="stat-label"><Icon name="check" size={12} />未变化</div>
              <div class="stat-value">{backupResult.unchanged}</div>
              <div class="stat-sub">跳过上传</div>
            </div>
            <div class="stat">
              <div class="stat-label"><Icon name="trash" size={12} />删除</div>
              <div class="stat-value">{backupResult.deleted}</div>
              <div class="stat-sub">源端已移除</div>
            </div>
            <div class="stat">
              <div class="stat-label"><Icon name="refresh" size={12} />清理孤儿</div>
              <div class="stat-value">{backupResult.orphan_removed}</div>
              <div class="stat-sub">保留策略</div>
            </div>
          </div>
        </div>
      {:else}
        <div class="alert alert-danger">
          <Icon name="x-circle" size={16} />
          <div class="alert-body">
            <div class="alert-title">备份失败</div>
            {backupResult.error}
          </div>
        </div>
      {/if}
    </div>
  {/if}
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .inline {
    margin-left: 6px;
  }
  .center {
    text-align: center;
  }
  .result-head {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--success);
    font-weight: 620;
    font-size: 14px;
    margin-bottom: var(--s3);
  }
</style>
