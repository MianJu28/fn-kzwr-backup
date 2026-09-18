<script>
  // WebDAV 凭据配置（ADR-009：官方 WebDAV，Basic 认证，凭据加密存储）
  export let configured = false;
  export let configuredUrl = '';
  export let busy = false;
  export let onSave = null; // (url, username, password) => Promise<string>

  let url = '';
  let username = '';
  let password = '';
  let saveMsg = '';

  async function submit() {
    const msg = await onSave(url, username, password);
    saveMsg = msg;
    if (msg && msg.startsWith('WebDAV 已配置')) {
      password = '';
      configuredUrl = url.trim().replace(/\/+$/, '');
      url = '';
    }
  }
</script>

<section>
  <h2>🌐 WebDAV 目标配置</h2>
  <p class="hint">备份目标为酷族网软（kzwr）官方 WebDAV。凭据先实测连通性，通过后加密存储。</p>
  {#if configured}
    <p class="ok">✅ 已配置：{configuredUrl || '（地址见配置文件）'}</p>
  {:else}
    <p class="warn">⚠️ 未配置，填写后才能执行备份/恢复</p>
  {/if}
  <label>WebDAV 地址
    <input bind:value={url} type="url" placeholder="https://dav.kzwr.com/dav" />
  </label>
  <label>用户名
    <input bind:value={username} type="text" placeholder="账号或邮箱" autocomplete="off" />
  </label>
  <label>密码 / 应用密码
    <input bind:value={password} type="password" placeholder="••••••••" autocomplete="new-password" />
  </label>
  <button on:click={submit} disabled={busy || !url || !username || !password}>
    {busy ? '验证并保存中...' : (configured ? '更新配置' : '测试并保存')}
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
  .hint { color: #5a6a7a; font-size: 13px; margin: 0 0 10px; }
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
