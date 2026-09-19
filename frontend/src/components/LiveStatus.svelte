<script>
  // 实时任务面板（WebSocket 推送）：常驻展示，空闲时展示引导态
  import { onDestroy } from 'svelte';
  import Icon from './Icon.svelte';
  import { fmtBytes, fmtBps, fmtDuration, pctOf, fmtInt } from '../lib/format.js';

  export let liveStatus = null;
  export let wsConnected = false;

  // 每秒刷新，使「用时」在两次事件之间也能连续走动
  let now = Date.now();
  const ticker = setInterval(() => (now = Date.now()), 1000);
  onDestroy(() => clearInterval(ticker));

  const STATUS = {
    started: { text: '已启动', icon: 'play' },
    progress: { text: '进行中', icon: 'activity' },
    completed: { text: '已完成', icon: 'check-circle' },
    failed: { text: '已失败', icon: 'x-circle' },
  };

  $: status = liveStatus ? STATUS[liveStatus.status] || { text: liveStatus.status, icon: 'info' } : null;
  $: running = !!liveStatus && (liveStatus.status === 'started' || liveStatus.status === 'progress');
  $: failed = !!liveStatus && liveStatus.status === 'failed';
  $: done = !!liveStatus && liveStatus.status === 'completed';
  $: pct = liveStatus ? pctOf(liveStatus.done, liveStatus.total) : 0;
  $: elapsed = liveStatus
    ? running
      ? (liveStatus.elapsed_ms || 0) + Math.max(0, now - (liveStatus.at || now))
      : liveStatus.elapsed_ms || 0
    : 0;
  $: kindIcon = liveStatus && liveStatus.kind === 'restore' ? 'download' : 'upload';
  $: kindText = liveStatus && liveStatus.kind === 'restore' ? '恢复' : '备份';
</script>

<section class="card live" class:active={running} class:done class:failed>
  <div class="card-head">
    {#if liveStatus}
      <div class="icon-wrap {running ? '' : failed ? 'danger' : 'ok'}">
        <Icon name={kindIcon} size={18} />
      </div>
    {:else}
      <div class="icon-wrap"><Icon name="activity" size={18} /></div>
    {/if}
    <div class="grow">
      <h2 class="card-title">实时任务</h2>
      <p class="card-desc">
        {#if liveStatus}{kindText}任务 · {status.text}{:else}通过 WebSocket 推送进度{/if}
      </p>
    </div>
  </div>

  <div class="card-body">
    {#if !liveStatus}
      <div class="empty">
        <div class="icon-wrap"><Icon name="clock" size={19} /></div>
        <strong>当前没有进行中的任务</strong>
        在「备份」页执行备份，或等待定时任务触发
      </div>
    {:else}
      <div class="progress-head">
        <span class="badge {running ? 'badge-info' : failed ? 'badge-danger' : 'badge-ok'}">
          <Icon name={status.icon} size={12} />{status.text}
        </span>
        {#if liveStatus.total}
          <span class="count mono">{liveStatus.done} / {liveStatus.total}</span>
        {/if}
      </div>

      <div class="progress">
        <div
          class="progress-fill {failed ? 'danger' : done ? 'ok' : ''} {!liveStatus.total && running
            ? 'indeterminate'
            : ''}"
          style="width: {pct}%"
        ></div>
      </div>

      {#if liveStatus.current_file}
        <div class="file" title={liveStatus.current_file}>
          <Icon name="file" size={13} />
          <span class="mono">{liveStatus.current_file}</span>
        </div>
      {/if}

      <div class="metrics">
        <div class="metric">
          <span class="metric-label">大小</span>
          <span class="metric-value mono">
            {fmtBytes(liveStatus.bytes_done)}{liveStatus.bytes_total > 0
              ? ` / ${fmtBytes(liveStatus.bytes_total)}`
              : ''}
          </span>
        </div>
        <div class="metric">
          <span class="metric-label">速度</span>
          <span class="metric-value mono">{running ? fmtBps(liveStatus.speed) : '—'}</span>
        </div>
        <div class="metric">
          <span class="metric-label">用时</span>
          <span class="metric-value mono">{fmtDuration(elapsed)}</span>
        </div>
        <div class="metric">
          <span class="metric-label">文件</span>
          <span class="metric-value mono">{fmtInt(liveStatus.done)}</span>
        </div>
      </div>

      {#if liveStatus.message}
        <div class="alert {failed ? 'alert-danger' : done ? 'alert-ok' : 'alert-info'} msg">
          <Icon name={failed ? 'alert' : done ? 'check-circle' : 'info'} size={15} />
          <div class="alert-body">{liveStatus.message}</div>
        </div>
      {/if}
    {/if}
  </div>

  <div class="card-foot conn">
    <span class="dot" class:on={wsConnected} class:off={!wsConnected} class:pulse={wsConnected}></span>
    <span>{wsConnected ? '实时通道已连接' : '实时通道断开，正在重连…'}</span>
  </div>
</section>

<style>
  .live.active {
    border-color: var(--primary-soft-border);
  }
  .live.failed {
    border-color: var(--danger-border);
  }
  .grow {
    flex: 1;
    min-width: 0;
  }

  .progress-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s2);
    margin-bottom: var(--s2);
  }
  .count {
    color: var(--text-2);
    font-size: 12px;
  }

  .file {
    display: flex;
    align-items: center;
    gap: 7px;
    margin-top: var(--s3);
    padding: 7px 9px;
    border-radius: var(--r-sm);
    background: var(--surface-3);
    border: 1px solid var(--border);
    color: var(--text-2);
    font-size: 11.5px;
    overflow: hidden;
  }
  .file span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .metrics {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--s2);
    margin-top: var(--s3);
  }
  .metric {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 9px 10px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .metric-label {
    font-size: 11px;
    color: var(--text-3);
  }
  .metric-value {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text);
    word-break: break-all;
  }
  .msg {
    margin-top: var(--s3);
    font-size: 12.5px;
  }
  .conn {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-3);
    font-size: 11.5px;
    padding: var(--s3) var(--s5);
  }
</style>
