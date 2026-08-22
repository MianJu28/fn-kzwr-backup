<script>
  // kzwr 登录
  export let loggedIn = false;
  export let busy = false;
  export let onLogin = null; // (username, password) => Promise
  // 登录二进制 debug 日志开关
  export let loginDebug = false;
  export let onToggleLoginDebug = null; // (bool) => Promise

  let username = '';
  let password = '';
  let loginMsg = '';

  async function submit() {
    const msg = await onLogin(username, password);
    loginMsg = msg;
    if (msg && msg.startsWith('已登录')) {
      password = '';
    }
  }

  async function toggleDebug() {
    if (onToggleLoginDebug) {
      await onToggleLoginDebug(!loginDebug);
    }
  }
</script>

<section>
  <h2>🔑 kzwr 登录</h2>
  <p class="hint">登录酷族网软（kzwr.com），凭据加密存储，token 过期自动重新登录。</p>
  {#if loggedIn}
    <p class="ok">✅ 已登录</p>
  {:else}
    <p class="warn">⚠️ 未登录，请填写凭据</p>
  {/if}
  <label>用户名（邮箱）
    <input bind:value={username} type="email" placeholder="you@example.com" />
  </label>
  <label>密码
    <input bind:value={password} type="password" placeholder="••••••••" />
  </label>
  <button on:click={submit} disabled={busy || !username || !password}>
    {busy ? '登录中...' : (loggedIn ? '更新凭据' : '登录 kzwr')}
  </button>
  {#if loginMsg}
    <p class:ok={loggedIn} class:warn={!loggedIn}>{loginMsg}</p>
  {/if}
  <label class="debug-toggle">
    <input type="checkbox" checked={loginDebug} on:change={toggleDebug} />
    开启登录日志（--debug，输出到 login_debug.log）
  </label>
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
  .debug-toggle {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 14px;
    font-size: 13px;
    color: #5a6a7a;
  }
  .debug-toggle input {
    width: auto;
    margin: 0;
  }
</style>
