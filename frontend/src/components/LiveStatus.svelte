<script>
  // 实时任务状态（WebSocket 推送）
  export let liveStatus = null; // { kind, status, current_file, done, total, message }

  function statusText(s) {
    const map = {
      started: '开始',
      progress: '进行中',
      completed: '完成',
      failed: '失败',
    };
    return map[s] || s;
  }
</script>

{#if liveStatus}
  <section class="live-status">
    <h2>{liveStatus.kind === 'backup' ? '⬆️' : '⬇️'} 实时任务</h2>
    <div class="ls-row">
      <span class="ls-label">状态</span>
      <span class="ls-value">{statusText(liveStatus.status)}</span>
    </div>
    {#if liveStatus.current_file}
      <div class="ls-row">
        <span class="ls-label">当前文件</span>
        <span class="ls-value file">{liveStatus.current_file}</span>
      </div>
    {/if}
    {#if liveStatus.total > 0}
      <div class="ls-row">
        <span class="ls-label">进度</span>
        <span class="ls-value">{liveStatus.done} / {liveStatus.total}</span>
      </div>
      <div class="progress-bar">
        <div class="progress-fill" style="width: {liveStatus.total ? (liveStatus.done / liveStatus.total * 100) : 0}%"></div>
      </div>
    {/if}
    {#if liveStatus.message}
      <div class="ls-row">
        <span class="ls-label">信息</span>
        <span class="ls-value">{liveStatus.message}</span>
      </div>
    {/if}
  </section>
{/if}

<style>
  .live-status { background: #f0fdf4; border: 1px solid #bbf7d0; }
  h2 { margin: 0 0 8px; font-size: 18px; }
  .ls-row {
    display: flex;
    gap: 12px;
    padding: 7px 0;
    font-size: 14px;
    border-bottom: 1px solid #e9f9ef;
  }
  .ls-row:last-child { border-bottom: none; }
  .ls-label { color: #42526e; font-weight: 600; width: 90px; flex-shrink: 0; }
  .ls-value { color: #1f2d3d; word-break: break-all; }
  .ls-value.file { font-family: monospace; }
  .progress-bar {
    height: 8px;
    background: #e5e7eb;
    border-radius: 4px;
    overflow: hidden;
    margin: 8px 0;
  }
  .progress-fill {
    height: 100%;
    background: #22c55e;
    transition: width 0.3s;
  }
</style>
