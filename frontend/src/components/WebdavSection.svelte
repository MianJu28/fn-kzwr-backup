<script>
  // WebDAV 凭据配置（地址固定为官方地址，保存前实测连通性后加密存储）
  import Icon from './Icon.svelte';

  export let configured = false;
  export let configuredUrl = '';
  /** 已保存的账号（非敏感，回填方便修改；密码永不回显） */
  export let configuredUsername = '';
  export let busy = false;
  export let onSave = null; // (username, password) => Promise<string>

  let username = '';
  let password = '';
  let saveMsg = '';
  let saveOk = false;
  let showPassword = false;

  const DEFAULT_URL = 'https://dav.kzwr.com/dav';

  // 仅在「后端回显值变化」时回填，避免用户输入被覆盖
  let lastUser = null;
  function syncUsername(u) {
    if (u === lastUser) return;
    lastUser = u;
    username = u || '';
  }
  $: syncUsername(configuredUsername || '');

  async function submit() {
    saveMsg = '';
    const msg = await onSave(username, password);
    saveMsg = msg || '';
    saveOk = !msg;
    if (!msg) password = '';
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="cloud" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">WebDAV 目标</h2>
      <p class="card-desc">
        固定使用 kzwr 官方 WebDAV：<code>{configuredUrl || DEFAULT_URL}</code>
      </p>
    </div>
    <span class="badge {configured ? 'badge-ok' : 'badge-warn'}">
      <span class="dot" class:on={configured} class:off={!configured}></span>
      {configured ? '已配置' : '未配置'}
    </span>
  </div>

  <div class="card-body">
    <div class="grid2">
      <label class="field">
        <span class="label">用户名 / 账号</span>
        <input
          class="input"
          bind:value={username}
          type="text"
          placeholder="账号或邮箱"
          autocomplete="off"
        />
        {#if configuredUsername}
          <span class="field-hint">当前已保存：<code>{configuredUsername}</code>，可直接沿用</span>
        {/if}
      </label>

      <label class="field">
        <span class="label">密码 / 应用密码</span>
        <div class="pwd">
          <input
            class="input"
            type={showPassword ? 'text' : 'password'}
            value={password}
            on:input={(e) => (password = e.currentTarget.value)}
            placeholder="••••••••"
            autocomplete="new-password"
          />
          <button
            class="btn-icon reveal"
            type="button"
            on:click={() => (showPassword = !showPassword)}
            aria-label={showPassword ? '隐藏密码' : '显示密码'}
          >
            <Icon name={showPassword ? 'eye-off' : 'eye'} size={15} />
          </button>
        </div>
        {#if configured}
          <span class="field-hint">出于安全不回显已保存密码；仅修改账号时仍需重新输入一次</span>
        {/if}
      </label>
    </div>

    <div class="alert alert-info">
      <Icon name="info" size={15} />
      <div class="alert-body">
        保存前会先实测连通性；通过后凭据加密存储并热切换目标，无需重启服务。大文件会自动分片上传。
      </div>
    </div>

    {#if saveMsg}
      <div class="alert {saveOk ? 'alert-ok' : 'alert-danger'} msg">
        <Icon name={saveOk ? 'check-circle' : 'x-circle'} size={15} />
        <div class="alert-body">{saveMsg}</div>
      </div>
    {/if}
  </div>

  <div class="card-foot foot">
    <span class="foot-hint">凭据仅保存在本机配置目录（age 加密）</span>
    <button class="btn btn-primary" on:click={submit} disabled={busy || !username || !password}>
      {#if busy}
        <span class="spin"></span>验证中…
      {:else}
        <Icon name="link" size={15} />{configured ? '更新凭据' : '测试并保存'}
      {/if}
    </button>
  </div>
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .grid2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--s3);
  }
  @media (max-width: 640px) {
    .grid2 {
      grid-template-columns: 1fr;
    }
  }
  .pwd {
    position: relative;
  }
  .pwd .input {
    padding-right: 38px;
  }
  .reveal {
    position: absolute;
    right: 4px;
    top: 50%;
    transform: translateY(-50%);
  }
  .msg {
    margin-top: var(--s3);
  }
  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s3);
    flex-wrap: wrap;
  }
  .foot-hint {
    color: var(--text-3);
    font-size: 12px;
  }
</style>
