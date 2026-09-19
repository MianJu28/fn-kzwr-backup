<script>
  // 告警卡片（备份/恢复/定时/配置异常）：默认折叠展示最新若干条
  import Icon from './Icon.svelte';
  import { fmtTime } from '../lib/format.js';

  export let alerts = [];
  export let onClear = null; // () => Promise<void>

  const SOURCE_TEXT = {
    backup: '备份',
    restore: '恢复',
    scheduler: '定时',
    config: '配置',
  };
  const COLLAPSED = 3;
  let expanded = false;

  $: hasError = alerts.some((a) => a.level === 'error');
  $: shown = expanded ? alerts : alerts.slice(0, COLLAPSED);
</script>

{#if alerts.length > 0}
  <section class="card alert-card" class:error={hasError}>
    <div class="card-head">
      <div class="icon-wrap {hasError ? 'danger' : 'warn'}">
        <Icon name="shield_alert" size={18} />
      </div>
      <div class="grow">
        <h2 class="card-title">
          {hasError ? '存在异常告警' : '提示告警'}
          <span class="badge {hasError ? 'badge-danger' : 'badge-warn'}">{alerts.length}</span>
        </h2>
        <p class="card-desc">备份/恢复失败或配置缺失会在此汇总，可在设置中配置 Webhook 外发</p>
      </div>
      <button class="btn btn-sm btn-ghost" on:click={onClear}>
        <Icon name="trash" size={13} />清空
      </button>
    </div>

    <ul class="list">
      {#each shown as a (a.id)}
        <li class:err={a.level === 'error'}>
          <span class="tag">{SOURCE_TEXT[a.source] || a.source}</span>
          <span class="msg">{a.message}</span>
          <span class="ts mono">{fmtTime(a.ts)}</span>
        </li>
      {/each}
    </ul>

    {#if alerts.length > COLLAPSED}
      <div class="card-foot">
        <button class="btn btn-sm btn-ghost" on:click={() => (expanded = !expanded)}>
          <Icon name={expanded ? 'chevron_down' : 'chevron_right'} size={13} />
          {expanded ? '收起' : `展开全部（${alerts.length}）`}
        </button>
      </div>
    {/if}
  </section>
{/if}

<style>
  .alert-card.error {
    border-color: var(--danger-border);
  }
  .card-head {
    align-items: flex-start;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .card-title {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0 var(--s5) var(--s5);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  li {
    display: flex;
    align-items: baseline;
    gap: 9px;
    flex-wrap: wrap;
    padding: 9px 11px;
    border-radius: var(--r-sm);
    background: var(--warn-soft);
    border: 1px solid var(--warn-border);
    color: var(--warn);
    font-size: 12.5px;
  }
  li.err {
    background: var(--danger-soft);
    border-color: var(--danger-border);
    color: var(--danger);
  }
  .tag {
    flex-shrink: 0;
    font-weight: 640;
    font-size: 11.5px;
  }
  .msg {
    flex: 1;
    min-width: 0;
    word-break: break-word;
    line-height: 1.5;
  }
  .ts {
    flex-shrink: 0;
    opacity: 0.75;
    font-size: 11px;
  }
</style>
