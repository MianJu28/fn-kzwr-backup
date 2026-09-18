<script>
  // 告警横幅（监控告警：备份/恢复/定时/配置异常）
  export let alerts = [];
  export let onClear = null; // () => Promise<void>

  const SOURCE_TEXT = {
    backup: '备份',
    restore: '恢复',
    scheduler: '定时',
    config: '配置',
  };

  function timeText(ts) {
    if (!ts) return '';
    try {
      return new Date(ts).toLocaleString();
    } catch (e) {
      return '';
    }
  }
</script>

{#if alerts.length > 0}
  <div class="alerts">
    <div class="alerts-head">
      <span class="title">⚠️ 告警（{alerts.length}）</span>
      <button on:click={onClear}>清空</button>
    </div>
    <ul>
      {#each alerts as a (a.id)}
        <li class:err={a.level === 'error'} class:warn={a.level !== 'error'}>
          <span class="tag">{SOURCE_TEXT[a.source] || a.source}</span>
          <span class="msg">{a.message}</span>
          <span class="ts">{timeText(a.ts)}</span>
        </li>
      {/each}
    </ul>
  </div>
{/if}

<style>
  .alerts {
    margin-top: 16px;
    background: #fef2f2;
    border: 1px solid #fecaca;
    border-radius: 10px;
    padding: 14px 16px;
  }
  .alerts-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 8px;
  }
  .title { font-weight: 600; color: #b91c1c; font-size: 14px; }
  button {
    background: #fff;
    color: #b91c1c;
    border: 1px solid #fca5a5;
    border-radius: 6px;
    padding: 5px 12px;
    font-size: 13px;
    cursor: pointer;
    flex-shrink: 0;
  }
  button:hover { background: #fee2e2; }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 180px;
    overflow-y: auto;
  }
  li {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 6px 0;
    font-size: 13px;
    border-top: 1px solid #fee2e2;
    flex-wrap: wrap;
  }
  li:first-child { border-top: none; }
  .tag {
    flex-shrink: 0;
    background: #fee2e2;
    color: #b91c1c;
    border-radius: 4px;
    padding: 1px 8px;
    font-size: 11px;
  }
  li.warn .tag { background: #fff7ed; color: #b45309; }
  .msg { flex: 1; min-width: 0; color: #7f1d1d; word-break: break-all; line-height: 1.5; }
  li.warn .msg { color: #92400e; }
  .ts { flex-shrink: 0; color: #9ca3af; font-size: 11px; }
</style>
