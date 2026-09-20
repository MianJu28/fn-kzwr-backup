<script>
  // 侧栏账号卡（实时任务上方）：账号、套餐与云端存储空间
  // 数据来自 kzwr 增强接口；未配置 access-token 时给出轻提示
  import Icon from './Icon.svelte';
  import { fmtBytes } from '../lib/format.js';

  export let username = '';
  export let configured = false;
  export let kzwr = null; // { name, email, plan, total, used, percentage, error }
  export let onGoto = null; // (pageId) => void

  $: name =
    (kzwr && (kzwr.name || kzwr.email)) || username || (configured ? '已配置' : '未配置');
  $: quota = kzwr && kzwr.total ? kzwr : null;
  $: pct = quota ? Math.min(100, Math.round((quota.used / quota.total) * 100)) : 0;
  $: over = quota && kzwr && kzwr.error ? false : pct >= 85;
</script>

<section class="card rail-card">
  <div class="head">
    <Icon name="user" size={13} />
    <span class="title">账号</span>
    {#if quota && quota.plan}
      <span class="plan">{quota.plan}</span>
    {/if}
    {#if kzwr && kzwr.error}
      <span class="err" title={kzwr.error}>
        <Icon name="alert" size={12} />
      </span>
    {/if}
  </div>

  <div class="name mono" title={name}>{name}</div>

  {#if quota}
    <div class="bar" class:over={over} aria-label="空间占用 {pct}%">
      <div class="fill" style="width:{pct}%"></div>
    </div>
    <div class="space">
      <span class="mono">{fmtBytes(quota.used)} / {fmtBytes(quota.total)}</span>
      <span class="pct" class:over={over}>{pct}%</span>
    </div>
  {:else if kzwr && kzwr.error}
    <div class="hint danger" title={kzwr.error}>access-token 已失效</div>
    {#if onGoto}
      <button class="link" on:click={() => onGoto('settings')}>去设置页更新</button>
    {/if}
  {:else if configured}
    <div class="hint">
      配置 access-token 后显示空间
      {#if onGoto}
        <button class="link" on:click={() => onGoto('settings')}>去配置</button>
      {/if}
    </div>
  {:else}
    <div class="hint">
      未配置 WebDAV
      {#if onGoto}
        <button class="link" on:click={() => onGoto('settings')}>去配置</button>
      {/if}
    </div>
  {/if}
</section>

<style>
  .rail-card {
    padding: 10px 12px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 5px;
    color: var(--text-3);
  }
  .title {
    font-size: 11.5px;
    font-weight: 600;
    letter-spacing: 0.02em;
  }
  .plan {
    margin-left: auto;
    font-size: 10.5px;
    padding: 1px 6px;
    border-radius: var(--r-full);
    background: var(--primary-soft);
    color: var(--primary);
  }
  .err {
    margin-left: auto;
    color: var(--warn);
    display: inline-flex;
  }
  .name {
    margin-top: 6px;
    font-size: 12px;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .bar {
    height: 5px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
    margin-top: 8px;
  }
  .bar .fill {
    height: 100%;
    background: var(--primary);
    transition: width var(--t-fast);
  }
  .bar.over .fill {
    background: var(--warn, #d97706);
  }
  .space {
    display: flex;
    justify-content: space-between;
    gap: 6px;
    margin-top: 5px;
    font-size: 11px;
    color: var(--text-3);
  }
  .pct.over {
    color: var(--warn);
    font-weight: 600;
  }
  .hint {
    margin-top: 7px;
    font-size: 11.5px;
    color: var(--text-3);
    line-height: 1.6;
  }
  .hint.danger {
    color: var(--warn);
  }
  .link {
    border: none;
    background: none;
    padding: 0;
    color: var(--primary);
    cursor: pointer;
    font-size: 11.5px;
    margin-left: 4px;
  }
  .link:hover {
    text-decoration: underline;
  }
</style>
