<script>
  // 消息提醒（统一消息中心）
  //
  // 汇总原则：
  // - **聚焦点即时反馈**（刚点完按钮的结果、字段校验）用 toast / 卡片内就地提示，保留；
  // - **需要留存、跨页可见的异常与风险**（备份/恢复失败、空间预警、token 失效、
  //   账号不一致、配置缺失）统一汇总到这里，不再在各卡片里各写一份重复提示。
  import Icon from './Icon.svelte';
  import { fmtTime } from '../lib/format.js';

  export let alerts = [];
  export let onClear = null; // () => Promise<void>

  const SOURCE_TEXT = {
    backup: '备份',
    restore: '恢复',
    scheduler: '定时',
    config: '配置',
    kzwr: '增强',
  };
  const COLLAPSED = 3;
  let expanded = false;

  $: errors = alerts.filter((a) => a.level === 'error').length;
  $: warns = alerts.length - errors;
  $: hasError = errors > 0;
  $: shown = expanded ? alerts : alerts.slice(0, COLLAPSED);
  $: summary = hasError
    ? `${errors} 条错误${warns ? ` · ${warns} 条警告` : ''}`
    : `${warns} 条警告`;
</script>

{#if alerts.length > 0}
  <section class="card alert-card" class:error={hasError}>
    <div class="card-head">
      <div class="icon-wrap {hasError ? 'danger' : 'warn'}">
        <Icon name="shield_alert" size={18} />
      </div>
      <div class="grow">
        <h2 class="card-title">
          消息提醒
          <span class="badge {hasError ? 'badge-danger' : 'badge-warn'}">{alerts.length}</span>
          <span class="sum">{summary}</span>
        </h2>
        <p class="card-desc">
          备份/恢复失败、空间预警、账号与配置异常都汇总在这里（情况恢复后会自动消失）；可在设置中配置 Webhook
          外发到手机或群机器人
        </p>
      </div>
      <button class="btn btn-sm btn-ghost" on:click={onClear}>
        <Icon name="trash" size={13} />清空
      </button>
    </div>

    <ul class="list">
      {#each shown as a (a.id)}
        <li class:err={a.level === 'error'}>
          <Icon name={a.level === 'error' ? 'x-circle' : 'alert'} size={13} />
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
  .sum {
    font-size: 11.5px;
    font-weight: 400;
    color: var(--text-3);
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
