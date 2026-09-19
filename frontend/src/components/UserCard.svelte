<script>
  // 备份账号卡片（官方 WebDAV 无套餐/容量接口，仅展示账号与连接状态）
  import Icon from './Icon.svelte';

  export let userInfo = null;
  export let userInfoError = null;
  export let configured = false;

  $: name = (userInfo && userInfo.username) || '';
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
      </div>
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
</style>
