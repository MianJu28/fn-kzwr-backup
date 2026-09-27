<script>
  /**
   * 安装插件弹窗
   *
   * 与「插件设置弹窗」同构：都用 `.scrim` + `.modal`，观感一致。
   *
   * 表单刻意**只保留必要项**：
   * - 插件文件 `.so`（必选）
   * - 签名文件 `.so.sig`（必选；宿主强制验签，缺签名无法安装）
   * - 该插件的公钥（必填，一插件一公钥）
   *
   * 安装文件名**不要求输入**：直接用所选 `.so` 的原文件名，
   * 少一个输入框就少一类「填了不存在的名字导致找不到文件」的问题。
   */
  import Icon from './Icon.svelte';

  /** 打开/关闭 */
  export let open = false;
  /** 关闭回调 */
  export let onClose = null;
  /** 提交回调：(soFile, sigFile, pubkey) => Promise<{error?, success?, note?}> */
  export let onSubmit = null;

  let so = null;
  let sig = null;
  let pubkey = '';
  let msg = '';
  let ok = false;
  let busy = false;

  function close() {
    if (busy) return;
    if (onClose) onClose();
  }

  function onKeydown(e) {
    if (e.key === 'Escape') close();
  }

  function pickSo(e) {
    const f = e.target.files && e.target.files[0];
    so = f || null;
    msg = '';
    ok = false;
  }

  function pickSig(e) {
    const f = e.target.files && e.target.files[0];
    sig = f || null;
    msg = '';
    ok = false;
  }

  async function submit() {
    msg = '';
    ok = false;
    if (!so) {
      msg = '请先选择插件文件（.so）';
      return;
    }
    if (!sig) {
      msg = '请同时选择签名文件（<插件名>.so.sig）—— 宿主强制验签，缺签名无法安装';
      return;
    }
    if (!pubkey.trim()) {
      msg = '请填写该插件的公钥（用签名私钥对应的公钥）';
      return;
    }
    busy = true;
    try {
      const r = onSubmit ? await onSubmit(so, sig, pubkey.trim()) : null;
      if (r && r.error) {
        msg = r.error;
        ok = false;
        return;
      }
      // 成功：由父组件负责提示与刷新，这里直接关闭并复位
      so = null;
      sig = null;
      pubkey = '';
      msg = '';
      close();
    } catch (e) {
      msg = e.message;
      ok = false;
    } finally {
      busy = false;
    }
  }

  function reset() {
    so = null;
    sig = null;
    pubkey = '';
    msg = '';
    ok = false;
    busy = false;
  }

  // 每次打开时复位，避免残留上一次的文件与提示
  $: if (open === false) reset();
</script>

<svelte:window on:keydown={onKeydown} />

{#if open}
  <div class="scrim" role="presentation" on:click|self={close}>
    <div class="modal plugin-modal" role="dialog" aria-modal="true" aria-label="安装插件">
      <div class="modal-title">
        <Icon name="package" size={18} />
        <span class="grow">安装插件</span>
        <button class="btn-icon" on:click={close} aria-label="关闭">
          <Icon name="x" size={16} />
        </button>
      </div>

      <div class="modal-body scroll-body">
        <p class="hint-top">
          选择插件文件与它对应的签名，并填写<strong>该插件的公钥</strong>。
          安装时会先用该公钥验签，<strong>校验不通过不会写入磁盘</strong>。
        </p>

        <div class="field">
          <label for="ins-so">插件文件（.so）</label>
          <input id="ins-so" type="file" accept=".so" on:change={pickSo} disabled={busy} />
          {#if so}
            <p class="field-hint">
              将安装为 <code>{so.name}</code>
            </p>
          {/if}
        </div>

        <div class="field">
          <label for="ins-sig">签名文件（.so.sig）</label>
          <input id="ins-sig" type="file" accept=".sig" on:change={pickSig} disabled={busy} />
          <p class="field-hint">
            用 <code>Scripts/sign_plugin.sh sign &lt;插件.so&gt;</code> 生成。
          </p>
        </div>

        <div class="field">
          <label for="ins-pub">该插件的公钥（base64 的 32 字节 Ed25519 公钥）</label>
          <input id="ins-pub" class="mono" placeholder="例如：YCzDjlN5uEHPulgwyGWnZYpYV3P7O1xPNpTT0zAkv+A="
            value={pubkey} on:input={(e) => (pubkey = e.target.value)} disabled={busy} />
          <p class="field-hint">
            用 <code>Scripts/sign_plugin.sh pubkey</code> 打印。
            <strong>一个插件只认它自己的公钥</strong>——其它插件的公钥无法通过校验。
          </p>
        </div>

        {#if msg}
          <p class={ok ? 'field-hint ok' : 'field-error'}>{msg}</p>
        {/if}
      </div>

      <div class="modal-actions">
        <button class="btn btn-ghost" on:click={close} disabled={busy}>取消</button>
        <button class="btn btn-primary" on:click={submit} disabled={busy}>
          {#if busy}<span class="spin"></span>校验并安装中…{:else}校验并安装{/if}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .plugin-modal {
    max-width: 560px;
    width: 100%;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .scroll-body {
    max-height: min(60vh, 520px);
    overflow-y: auto;
    padding-right: 2px;
  }
  .hint-top {
    margin: 0 0 var(--s3);
    font-size: 12.5px;
    line-height: 1.6;
    color: var(--text-3);
  }
  /* 原生 file input 很突兀，统一外观 */
  input[type='file'] {
    padding: 7px 10px;
    font-size: 12.5px;
    color: var(--text-2);
    background: var(--surface);
    cursor: pointer;
  }
  input[type='file']::file-selector-button {
    margin-right: 10px;
    padding: 5px 12px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-sm);
    background: var(--surface-3);
    color: var(--text);
    font-family: inherit;
    font-size: 12.5px;
    cursor: pointer;
    transition: background var(--t-fast);
  }
  input[type='file']::file-selector-button:hover {
    background: var(--surface-hover);
  }
  .field-hint.ok {
    color: var(--success);
  }
  .spin {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 2px solid currentColor;
    border-right-color: transparent;
    animation: spin 0.7s linear infinite;
    display: inline-block;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
