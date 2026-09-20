<script>
  // 侧栏账号卡（实时任务上方）：账号信息与云端存储空间的**唯一**展示位
  // 数据来自 kzwr 增强接口；未配置 access-token 时给出轻提示；支持手动刷新
  import Icon from './Icon.svelte';
  import { fmtBytes } from '../lib/format.js';

  export let username = '';
  export let configured = false;
  export let kzwr = null; // { name, email, plan, total, used, percentage, max_file_size, country, error }
  export let onGoto = null; // (pageId) => void
  export let onRefresh = null; // () => Promise<void>
  export let refreshing = false;

  $: name =
    (kzwr && (kzwr.name || kzwr.email)) || username || (configured ? '已配置' : '未配置');
  $: email = kzwr && kzwr.email && kzwr.email !== name ? kzwr.email : '';
  $: quota = kzwr && kzwr.total ? kzwr : null;
  $: pct = quota ? Math.min(100, Math.round((quota.used / quota.total) * 100)) : 0;
  $: over = quota && kzwr && kzwr.error ? false : pct >= 85;
  $: hasDetail = !!(kzwr && !kzwr.error && (email || quota || kzwr.max_file_size || kzwr.country));
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
    {#if onRefresh}
      <button
        class="refresh"
        on:click={onRefresh}
        disabled={refreshing}
        title={refreshing ? '刷新中…' : '刷新账号信息'}
        aria-label="刷新账号信息"
      >
        <span class="spin {refreshing ? 'on' : ''}"><Icon name="refresh" size={12} /></span>
      </button>
    {/if}
  </div>

  <div class="name mono" title={name}>{name}</div>
  {#if email}
    <div class="sub mono" title={email}>{email}</div>
  {/if}

  {#if quota}
    <div class="bar" class:over={over} aria-label="空间占用 {pct}%">
      <div class="fill" style="width:{pct}%"></div>
    </div>
    <div class="space">
      <span class="mono">{fmtBytes(quota.used)} / {fmtBytes(quota.total)}</span>
      <span class="pct" class:over={over}>{pct}%</span>
    </div>
    {#if kzwr.max_file_size}
      <div class="kv"><span>单文件上限</span><span class="mono">{fmtBytes(kzwr.max_file_size)}</span></div>
    {/if}
    {#if kzwr.country}
      <div class="kv"><span>地区</span><span>{kzwr.country}</span></div>
    {/if}
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
  .refresh {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    border: none;
    border-radius: var(--r-xs);
    background: transparent;
    color: var(--text-3);
    cursor: pointer;
    padding: 0;
  }
  .plan ~ .refresh,
  .err ~ .refresh {
    margin-left: 2px;
  }
  .refresh:hover:not(:disabled) {
    background: var(--surface-3);
    color: var(--text);
  }
  .refresh:disabled {
    cursor: default;
    opacity: 0.7;
  }
  .spin {
    display: inline-flex;
  }
  .spin.on :global(svg) {
    animation: rotate 0.9s linear infinite;
  }
  @keyframes rotate {
    to {
      transform: rotate(360deg);
    }
  }
  .name {
    margin-top: 6px;
    font-size: 12px;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sub {
    margin-top: 2px;
    font-size: 10.5px;
    color: var(--text-3);
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
  .kv {
    display: flex;
    justify-content: space-between;
    gap: 6px;
    margin-top: 4px;
    font-size: 10.5px;
    color: var(--text-3);
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
