<script>
  // WebDAV 凭据配置（ADR-009：官方 WebDAV，Basic 认证，凭据加密存储）
  // 地址固定为官方 WebDAV，无需用户设置
  export let configured = false;
  export let configuredUrl = '';
  export let busy = false;
  export let onSave = null; // (username, password) => Promise<string>

  let username = '';
  let password = '';
  let saveMsg = '';

  async function submit() {
    const msg = await onSave(username, password);
    saveMsg = msg;
    if (msg && msg.startsWith('WebDAV 已配置')) {
      password = '';
    }
  }
</script>

<section>
  <h2>🌐 WebDAV 目标配置</h2>
  <p class="hint">
    备份目标为酷族网软（kzwr）官方 WebDAV：<code>{configuredUrl || 'https://dav.kzwr.com/dav'}</code>
    <br />凭据保存前会先实测连通性，通过后加密存储；大文件自动分片上传。
  </p>
  {#if configured}
    <p class="ok">✅ 已配置</p>
  {:else}
    <p class="warn">⚠️ 未配置，填写后才能执行备份/恢复</p>
  {/if}
  <label>用户名
    <input bind:value={username} type="text" placeholder="账号或邮箱" autocomplete="off" />
  </label>
  <label>密码 / 应用密码
    <input bind:value={password} type="password" placeholder="••••••••" autocomplete="new-password" />
  </label>
  <button on:click={submit} disabled={busy || !username || !password}>
    {busy ? '验证并保存中...' : (configured ? '更新凭据' : '测试并保存')}
  </button>
  {#if saveMsg}
    <p class:ok={saveMsg.startsWith('WebDAV 已配置')} class:warn={!saveMsg.startsWith('WebDAV 已配置')}>{saveMsg}</p>
  {/if}
</section>

<style>
  section {
    background: #fff;
    border-radius: 10px;
    padding: 20px;
    box-shadow: 0 1px 3px rgba(0,0,0,.06);
  }
  h2 { margin: 0 0 8px; font-size: 18px; }
  .hint { color: #5a6a7a; font-size: 13px; margin: 0 0 10px; line-height: 1.6; }
  .hint code {
    background: #eef2ff;
    color: #2563eb;
    border-radius: 4px;
    padding: 1px 6px;
    font-size: 12px;
  }
  .ok { color: #22a06b; }
  .warn { color: #b45309; }
  button {
    background: #2563eb;
    color: #fff;
    border: none;
    border-radius: 6px;
    padding: 10px 20px;
    font-size: 15px;
    cursor: pointer;
    margin-top: 12px;
  }
  button:disabled { background: #9db4e8; cursor: not-allowed; }
  label { display: block; margin: 12px 0 4px; font-size: 14px; color: #42526e; }
  input {
    width: 100%;
    padding: 8px 10px;
    border: 1px solid #d0d7e2;
    border-radius: 6px;
    margin-top: 4px;
    font-size: 14px;
    box-sizing: border-box;
  }
</style>
