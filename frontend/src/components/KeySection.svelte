<script>
  // age 密钥管理（ADR-003）：查看公钥 / 使用自定义私钥 / 自动生成并提醒保存
  export let keyInfo = null; // { public_key }
  export let busy = false;
  // 首次启动自动生成的私钥（后端一次性下发，提醒用户保存）
  export let revealKey = '';
  export let onSetKey = null; // (privateKey) => Promise<{success, error}>
  export let onGenerateKey = null; // () => Promise<{success, private_key, public_key, error}>

  let privateKeyInput = '';
  let msg = '';
  let msgOk = false;
  let generatedKey = revealKey || '';
  let copied = '';
  let working = false; // 本地忙碌态（不改写传入的 busy）

  $: if (revealKey) generatedKey = revealKey;

  async function copyText(text, tag) {
    if (!text) return;
    try {
      if (navigator.clipboard && window.isSecureContext) {
        await navigator.clipboard.writeText(text);
      } else {
        // 非安全上下文（http）下的兜底
        const ta = document.createElement('textarea');
        ta.value = text;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        document.execCommand('copy');
        document.body.removeChild(ta);
      }
      copied = tag;
      setTimeout(() => (copied = ''), 2000);
    } catch (e) {
      msg = '复制失败，请手动选中复制';
      msgOk = false;
    }
  }

  async function saveCustom() {
    if (!privateKeyInput.trim()) return;
    if (
      !confirm(
        '确定使用此私钥？\n\n更换私钥后，此前用旧公钥加密的备份数据将无法解密恢复！'
      )
    )
      return;
    working = true;
    const r = await onSetKey(privateKeyInput.trim());
    working = false;
    if (r.success) {
      msg = '私钥已更新并立即生效';
      msgOk = true;
      privateKeyInput = '';
      generatedKey = '';
    } else {
      msg = `设置失败: ${r.error}`;
      msgOk = false;
    }
  }

  async function generate() {
    if (
      !confirm(
        '将生成全新密钥对并替换当前密钥。\n\n此前备份的数据将无法解密恢复，确定继续？'
      )
    )
      return;
    working = true;
    const r = await onGenerateKey();
    working = false;
    if (r.success) {
      generatedKey = r.private_key;
      msg = '已生成新密钥对，请立即保存下方私钥';
      msgOk = true;
    } else {
      msg = `生成失败: ${r.error}`;
      msgOk = false;
    }
  }
</script>

<section>
  <h2>🔑 加密密钥</h2>
  <p class="hint">
    备份用 age 公钥加密，恢复需对应私钥。私钥经应用口令加密存储于配置目录，<strong>请自行另存备份</strong>——口令与私钥同时丢失将无法恢复数据。
  </p>

  <label>当前公钥
    <div class="key-row">
      <input readonly value={keyInfo?.public_key || ''} />
      <button class="ghost" on:click={() => copyText(keyInfo?.public_key || '', 'pub')}>
        {copied === 'pub' ? '已复制' : '复制'}
      </button>
    </div>
  </label>

  <label>自定义私钥（AGE-SECRET-KEY-1…，留空则沿用当前/自动生成的密钥）
    <textarea rows="2" bind:value={privateKeyInput} placeholder="AGE-SECRET-KEY-1..."></textarea>
  </label>

  <div class="btn-row">
    <button on:click={saveCustom} disabled={busy || working || !privateKeyInput.trim()}>
      {working ? '处理中...' : '使用此私钥'}
    </button>
    <button class="ghost" on:click={generate} disabled={busy || working}>
      {working ? '处理中...' : '自动生成新密钥'}
    </button>
  </div>

  {#if generatedKey}
    <div class="generated">
      <p class="warn strong">⚠️ 请立即保存以下私钥（仅显示这一次）：</p>
      <div class="key-row">
        <input readonly value={generatedKey} />
        <button class="ghost" on:click={() => copyText(generatedKey, 'gen')}>
          {copied === 'gen' ? '已复制' : '复制'}
        </button>
      </div>
    </div>
  {/if}

  {#if msg}
    <p class:ok={msgOk} class:warn={!msgOk}>{msg}</p>
  {/if}
</section>

<style>
  section {
    background: #fff;
    border-radius: 10px;
    padding: 20px;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.06);
  }
  h2 { margin: 0 0 8px; font-size: 18px; }
  .hint {
    color: #5a6a7a;
    font-size: 13px;
    line-height: 1.6;
    margin: 0 0 14px;
  }
  label { display: block; margin: 14px 0 4px; font-size: 14px; color: #42526e; }
  .key-row { display: flex; gap: 8px; margin-top: 6px; }
  input,
  textarea {
    flex: 1;
    min-width: 0;
    padding: 8px 10px;
    border: 1px solid #d0d7e2;
    border-radius: 6px;
    font-size: 13px;
    font-family: monospace;
    box-sizing: border-box;
    background: #fbfcfe;
  }
  textarea { width: 100%; resize: vertical; }
  .btn-row { display: flex; gap: 10px; flex-wrap: wrap; margin-top: 12px; }
  button {
    background: #2563eb;
    color: #fff;
    border: none;
    border-radius: 6px;
    padding: 9px 16px;
    font-size: 14px;
    cursor: pointer;
    flex-shrink: 0;
  }
  button:disabled { background: #9db4e8; cursor: not-allowed; }
  button.ghost {
    background: #eef2ff;
    color: #2563eb;
    flex-shrink: 0;
  }
  .generated {
    margin-top: 14px;
    padding: 14px;
    background: #fffbeb;
    border: 1px solid #fde68a;
    border-radius: 8px;
  }
  .generated p { margin: 0 0 8px; font-size: 13px; }
  .strong { font-weight: 600; }
  .ok { color: #22a06b; font-size: 13px; }
  .warn { color: #b45309; font-size: 13px; }
</style>
