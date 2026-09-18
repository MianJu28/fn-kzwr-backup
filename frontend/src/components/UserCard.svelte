<script>
  // 账号卡片：展示 WebDAV 配置账号（官方 WebDAV 无套餐/容量接口，仅展示账号状态）
  export let userInfo = null;
  export let userInfoError = null;
  export let configured = false;
</script>

<section class="user-card">
  <h2>👤 备份账号</h2>

  {#if userInfoError}
    <p class="warn">⚠️ {userInfoError}</p>
  {:else if !configured}
    <p class="warn">⚠️ WebDAV 未配置，请填写用户名与凭据</p>
  {:else}
    <div class="user-head">
      <div class="avatar-placeholder">👤</div>
      <div class="user-meta">
        <div class="user-name">{(userInfo && userInfo.username) || '已配置'}</div>
        <div class="user-email">kzwr 官方 WebDAV</div>
      </div>
    </div>
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
  .user-head { display: flex; align-items: center; gap: 14px; }
  .avatar-placeholder {
    width: 48px;
    height: 48px;
    border-radius: 50%;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 24px;
    background: #e6f4ff;
  }
  .user-meta { min-width: 0; }
  .user-name { font-size: 16px; font-weight: 600; color: #1f2d3d; word-break: break-all; }
  .user-email { font-size: 13px; color: #5a6a7a; margin-top: 2px; }
</style>
