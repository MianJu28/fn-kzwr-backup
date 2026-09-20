<script>
  // WebDAV 凭据配置（地址固定为官方地址，保存前实测连通性后加密存储）
  import Icon from './Icon.svelte';

  export let configured = false;
  export let configuredUrl = '';
  /** 已保存的账号（非敏感，回填方便修改；密码永不回显） */
  export let configuredUsername = '';
  export let busy = false;
  /** 保存后的提醒（如与 access-token 所属账号不一致） */
  export let warning = '';
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
        <span class="label">账号</span>
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
        <span class="label">应用密码</span>
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
          <span class="field-hint">出于安全不回显已保存的应用密码；仅修改账号时仍需重新输入一次</span>
        {/if}
      </label>
    </div>

    <details class="guide" open={!configured}>
      <summary>如何创建应用密码？（推荐：永不过期 + 读写权限）</summary>
      <ol>
        <li>
          浏览器打开
          <a href="https://www.kzwr.com/account/apps" target="_blank" rel="noreferrer">
            https://www.kzwr.com/account/apps <Icon name="external" size={11} />
          </a>
          并登录你的酷族账号
        </li>
        <li>点击「创建应用」/「新增应用密码」</li>
        <li><strong>权限选择「读写」</strong>（只读会导致上传备份失败）</li>
        <li><strong>有效期选择「永不过期」</strong>（否则密码到期后备份会中断）</li>
        <li>复制生成的密码，粘贴到上方「应用密码」输入框，账号填写同一个酷族账号</li>
      </ol>
      <p class="guide-note">
        提示：这里不要填账号登录密码，应用密码可随时在同一个页面吊销，更安全。
      </p>
    </details>

    {#if warning}
      <div class="alert alert-warn">
        <Icon name="alert" size={15} />
        <div class="alert-body">{warning}</div>
      </div>
    {/if}

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
  .guide {
    margin-top: var(--s3);
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    padding: 9px 12px;
    background: var(--surface-2);
  }
  .guide summary {
    cursor: pointer;
    font-size: 12.5px;
    font-weight: 560;
    color: var(--text-2);
  }
  .guide ol {
    margin: var(--s2) 0 0;
    padding-left: 20px;
    color: var(--text-2);
    font-size: 12.5px;
    line-height: 1.85;
  }
  .guide a {
    color: var(--primary);
    display: inline-flex;
    align-items: center;
    gap: 3px;
    word-break: break-all;
  }
  .guide-note {
    margin: var(--s2) 0 0;
    font-size: 12px;
    color: var(--text-3);
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
