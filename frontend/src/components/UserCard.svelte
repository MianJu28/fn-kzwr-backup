<script>
  // 用户信息卡片：展示登录用户 + 存储容量
  export let userInfo = null;
  export let userInfoError = null;

  // 字节数格式化
  function fmtBytes(b) {
    if (!b) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0;
    let n = b;
    while (n >= 1024 && i < units.length - 1) {
      n /= 1024;
      i++;
    }
    return `${n.toFixed(i === 0 ? 0 : 2)} ${units[i]}`;
  }

  // 已用百分比（0-100）
  $: percent = userInfo && userInfo.total > 0 ? Math.min(100, (userInfo.use_bytes / userInfo.total) * 100) : 0;
</script>

<section class="user-card">
  <h2>👤 登录用户</h2>

  {#if userInfoError}
    <p class="warn">⚠️ {userInfoError}</p>
  {:else if !userInfo || (!userInfo.email && !userInfo.logged_in_username)}
    <p class="hint">尚未登录 kzwr</p>
  {:else}
    <div class="user-head">
      {#if userInfo.avatar}
        <img class="avatar" src={userInfo.avatar} alt="avatar" />
      {:else}
        <div class="avatar-placeholder">👤</div>
      {/if}
      <div class="user-meta">
        <div class="user-name">{userInfo.name || userInfo.email || userInfo.logged_in_username}</div>
        <div class="user-email">{userInfo.email || userInfo.logged_in_username || ''}</div>
        {#if userInfo.plan}
          <div class="user-plan">套餐：{userInfo.plan}</div>
        {/if}
      </div>
    </div>

    {#if userInfo.total > 0}
      <div class="storage">
        <div class="storage-row">
          <span class="storage-label">存储空间</span>
          <span class="storage-value">
            {fmtBytes(userInfo.use_bytes)} / {fmtBytes(userInfo.total)}
            <span class="pct">（{userInfo.percentage || percent.toFixed(2) + '%'}）</span>
          </span>
        </div>
        <div class="storage-bar">
          <div class="storage-fill" class:warn={percent >= 80} class:full={percent >= 95} style="width: {percent}%"></div>
        </div>
      </div>
    {/if}
  {/if}
</section>

<style>
  section {
    background: #fff;
    border-radius: 10px;
    padding: 20px;
    margin-top: 16px;
    box-shadow: 0 1px 3px rgba(0,0,0,.06);
  }
  h2 { margin: 0 0 12px; font-size: 18px; }
  .warn { color: #b45309; }
  .hint { color: #5a6a7a; font-size: 14px; }
  .user-head { display: flex; align-items: center; gap: 14px; margin-bottom: 14px; }
  .avatar, .avatar-placeholder {
    width: 48px;
    height: 48px;
    border-radius: 50%;
    object-fit: cover;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 24px;
    background: #e6f4ff;
  }
  .user-meta { min-width: 0; }
  .user-name { font-size: 16px; font-weight: 600; color: #1f2d3d; }
  .user-email { font-size: 13px; color: #5a6a7a; margin-top: 2px; word-break: break-all; }
  .user-plan { font-size: 12px; color: #2563eb; margin-top: 4px; }
  .storage { border-top: 1px solid #eef1f6; padding-top: 12px; }
  .storage-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 13px;
    margin-bottom: 8px;
  }
  .storage-label { color: #42526e; font-weight: 600; }
  .storage-value { color: #1f2d3d; }
  .pct { color: #8a94a6; }
  .storage-bar {
    height: 8px;
    background: #e5e7eb;
    border-radius: 4px;
    overflow: hidden;
  }
  .storage-fill {
    height: 100%;
    background: #22c55e;
    transition: width 0.3s;
  }
  .storage-fill.warn { background: #f59e0b; }
  .storage-fill.full { background: #ef4444; }
</style>
