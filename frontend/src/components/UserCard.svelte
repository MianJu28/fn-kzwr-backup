<script>
  // 备份账号卡片（WebDAV 本身无套餐/容量接口；配置 access-token 后可显示存储空间）
  import Icon from './Icon.svelte';
  import { fmtBytes } from '../lib/format.js';

  export let userInfo = null;
  export let userInfoError = null;
  export let configured = false;
  /** kzwr 增强信息（可选）：{ plan, total, used, percentage } */
  export let kzwr = null;

  $: name = (userInfo && userInfo.username) || '';
  $: quota = kzwr && kzwr.total ? kzwr : null;
  $: pct = quota ? Math.min(100, Math.round((quota.used / quota.total) * 100)) : 0;
</script>

<section class="card">
  <div class="card-head">
    <div class="avatar" class:ok={configured}>
      <Icon name="user" size={20} />
    </div>
    <div class="meta">
      <h2 class="card-title">备份账号</h2>
      <p class="card-desc">酷族网软（kzwr）官方 WebDAV · Basic 认证</p>
    </div>
    <span class="badge {configured ? 'badge-ok' : 'badge-warn'}">
      <span class="dot" class:on={configured} class:off={!configured}></span>
      {configured ? '已连接' : '未配置'}
    </span>
  </div>

  <div class="card-body">
    {#if userInfoError}
      <div class="alert alert-warn">
        <Icon name="alert" size={16} />
        <div class="alert-body">{userInfoError}</div>
      </div>
    {:else if !configured}
      <div class="alert alert-warn">
        <Icon name="alert" size={16} />
        <div class="alert-body">尚未配置 WebDAV 凭据，备份与恢复暂不可用。</div>
      </div>
    {:else}
      <div class="rows">
        <div class="kv">
          <span class="k">账号</span>
          <span class="v mono">{name || '已配置'}</span>
        </div>
        <div class="kv">
          <span class="k">服务</span>
          <span class="v">kzwr 官方 WebDAV</span>
        </div>
        {#if quota}
          <div class="kv">
            <span class="k">套餐</span>
            <span class="v">{quota.plan || '—'}</span>
          </div>
          <div class="kv">
            <span class="k">存储空间</span>
            <span class="v">
              {fmtBytes(quota.used)} / {fmtBytes(quota.total)}
              {#if quota.percentage}<span class="dim">（{quota.percentage}）</span>{/if}
            </span>
          </div>
        {:else}
          <div class="kv">
            <span class="k">存储空间</span>
            <span class="v dim">配置 access-token 后显示</span>
          </div>
        {/if}
      </div>
      {#if quota}
        <div class="bar" aria-label="空间占用 {pct}%">
          <div class="fill" style="width:{pct}%"></div>
        </div>
      {/if}
    {/if}
  </div>
</section>

<style>
  .card-head {
    align-items: center;
  }
  .avatar {
    display: grid;
    place-items: center;
    width: 42px;
    height: 42px;
    border-radius: var(--r-full);
    background: var(--surface-3);
    color: var(--text-3);
    border: 1px solid var(--border);
    flex-shrink: 0;
  }
  .avatar.ok {
    background: var(--success-soft);
    color: var(--success);
    border-color: var(--success-border);
  }
  .meta {
    flex: 1;
    min-width: 0;
  }
  .rows {
    display: flex;
    flex-direction: column;
  }
  .dim {
    color: var(--text-3);
    font-size: 12px;
  }
  .bar {
    height: 7px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
    margin-top: var(--s3);
  }
  .bar .fill {
    height: 100%;
    background: var(--primary);
    transition: width var(--t-fast);
  }
</style>
