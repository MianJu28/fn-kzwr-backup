<script>
  // 实时任务状态（WebSocket 推送）；面板常驻显示，无任务时展示空闲态
  export let liveStatus = null; // { kind, status, current_file, done, total, message }
  export let wsConnected = false;

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

<section class="live-status" class:idle={!liveStatus}>
  <h2>{liveStatus ? (liveStatus.kind === 'backup' ? '⬆️' : '⬇️') : '⚡'} 实时任务</h2>
  <div class="conn" class:on={wsConnected}>
    <span class="dot"></span>{wsConnected ? '实时连接正常' : '实时连接中断'}
  </div>

  {#if !liveStatus}
    <p class="idle-text">当前没有进行中的任务</p>
  {:else}
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
        <div
          class="progress-fill"
          style="width: {liveStatus.total ? (liveStatus.done / liveStatus.total * 100) : 0}%"
        ></div>
      </div>
    {/if}
    {#if liveStatus.message}
      <div class="ls-row">
        <span class="ls-label">信息</span>
        <span class="ls-value">{liveStatus.message}</span>
      </div>
    {/if}
  {/if}
</section>

<style>
  .live-status {
    background: #fff;
    border: 1px solid #e0e4ea;
    border-radius: 10px;
    padding: 22px 20px;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.06);
  }
  .live-status:not(.idle) {
    background: #f0fdf4;
    border-color: #bbf7d0;
  }
  h2 { margin: 0 0 16px; font-size: 16px; line-height: 1.4; }
  .conn {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: #8a94a6;
    margin-bottom: 16px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #d1d5db;
    flex-shrink: 0;
  }
  .conn.on .dot { background: #22c55e; }
  .idle-text { color: #8a94a6; font-size: 13px; margin: 0; }
  .ls-row {
    display: flex;
    gap: 12px;
    padding: 7px 0;
    font-size: 13px;
    border-bottom: 1px solid #e9f9ef;
  }
  .ls-row:last-child { border-bottom: none; }
  .ls-label { color: #42526e; font-weight: 600; width: 68px; flex-shrink: 0; }
  .ls-value { color: #1f2d3d; word-break: break-all; min-width: 0; }
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
