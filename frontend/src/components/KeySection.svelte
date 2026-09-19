<script>
  // age 密钥管理（ADR-003）：查看公钥 / 使用自定义私钥 / 自动生成 / 导出备份
  export let keyInfo = null; // { public_key }
  export let busy = false;
  // 首次启动自动生成的私钥（后端一次性下发，提醒用户保存）
  export let revealKey = '';
  // 用户是否已确认备份私钥
  export let backedUp = false;
  export let onSetKey = null; // (privateKey) => Promise<{success, error}>
  export let onGenerateKey = null; // () => Promise<{success, private_key, public_key, error}>
  export let onExportKey = null; // (passphrase) => Promise<{private_key, error}>
  export let onBackupAck = null; // () => Promise<void>

  let privateKeyInput = '';
  let adminPassphrase = ''; // 显示私钥时输入的管理员口令
  let showExportBox = false; // 是否展开「显示私钥」的口令输入
  let msg = '';
  let msgOk = false;
  let shownKey = revealKey || ''; // 当前展示的私钥（生成/导出）
  let shownTag = revealKey ? 'new' : ''; // new=新生成；export=导出的当前私钥
  let copied = '';
  let working = false;

  $: if (revealKey) {
    shownKey = revealKey;
    shownTag = 'new';
  }

  async function copyText(text, tag) {
    if (!text) return;
    try {
      if (navigator.clipboard && window.isSecureContext) {
        await navigator.clipboard.writeText(text);
      } else {
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
    if (!confirm('确定使用此私钥？\n\n更换私钥后，此前用旧公钥加密的备份数据将无法解密恢复！')) return;
    working = true;
    const r = await onSetKey(privateKeyInput.trim());
    working = false;
    if (r.success) {
      msg = '私钥已更新并立即生效（该私钥由你提供，已标记为已备份）';
      msgOk = true;
      privateKeyInput = '';
      shownKey = '';
      shownTag = '';
    } else {
      msg = `设置失败: ${r.error}`;
      msgOk = false;
    }
  }

  async function generate() {
    if (!confirm('将生成全新密钥对并替换当前密钥。\n\n此前备份的数据将无法解密恢复，确定继续？')) return;
    working = true;
    const r = await onGenerateKey();
    working = false;
    if (r.success) {
      shownKey = r.private_key;
      shownTag = 'new';
      msg = '已生成新密钥对，请立即保存下方私钥';
      msgOk = true;
    } else {
      msg = `生成失败: ${r.error}`;
      msgOk = false;
    }
  }

  // 点击「显示私钥」：展开口令输入框（要求先输入管理员口令）
  function startExport() {
    msg = '';
    showExportBox = true;
  }

  function cancelExport() {
    showExportBox = false;
    adminPassphrase = '';
  }

  // 口令校验通过后展示私钥
  async function exportKey() {
    if (!adminPassphrase.trim()) {
      msg = '请输入管理员口令';
      msgOk = false;
      return;
    }
    if (!confirm('将显示当前私钥明文。\n\n请勿在公共场所或截图中泄露，确认继续？')) return;
    working = true;
    const r = await onExportKey(adminPassphrase.trim());
    working = false;
    if (r.private_key) {
      shownKey = r.private_key;
      shownTag = 'export';
      showExportBox = false;
      adminPassphrase = '';
      msg = '已显示当前私钥，请妥善保存到安全位置';
      msgOk = true;
    } else {
      msg = `导出失败: ${r.error}`;
      msgOk = false;
    }
  }

  // 确认已妥善保存：隐藏风险提醒与已展示的私钥
  async function ackBackup() {
    working = true;
    await onBackupAck();
    working = false;
    shownKey = '';
    shownTag = '';
    showExportBox = false;
    adminPassphrase = '';
    msg = '已确认保存，风险提醒已关闭';
    msgOk = true;
  }
</script>

<section>
  <h2>🔑 加密密钥</h2>
  <p class="hint">
    备份用 age 公钥加密，恢复需对应私钥。私钥经应用口令加密存储于配置目录，<strong>请自行另存备份</strong>——口令与私钥同时丢失将无法恢复数据。
  </p>

  {#if !backedUp}
    <div class="risk">
      ⚠️ 尚未确认备份私钥：私钥一旦丢失，已备份的数据将永久无法恢复。请先「显示私钥」保存到安全位置，再点「我已妥善保存」。
    </div>
  {/if}

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
    <button on:click={saveCustom} disabled={busy || working || !privateKeyInput.trim()}>使用此私钥</button>
    <button class="ghost" on:click={generate} disabled={busy || working}>自动生成新密钥</button>
    <button class="ghost" on:click={startExport} disabled={busy || working}>显示私钥</button>
    <button class="ok" on:click={ackBackup} disabled={busy || working || backedUp}>
      {backedUp ? '✓ 已确认备份' : '我已妥善保存'}
    </button>
  </div>

  {#if showExportBox}
    <div class="export-box">
      <p class="warn strong">显示私钥需先验证管理员口令：</p>
      <label>管理员口令
        <input
          type="password"
          bind:value={adminPassphrase}
          autocomplete="off"
          placeholder="安装时设置的管理员口令"
        />
      </label>
      <div class="btn-row">
        <button on:click={exportKey} disabled={working || !adminPassphrase.trim()}>
          {working ? '校验中...' : '确认显示'}
        </button>
        <button class="ghost" on:click={cancelExport} disabled={working}>取消</button>
      </div>
    </div>
  {/if}

  {#if shownKey}
    <div class="generated">
      <p class="warn strong">
        ⚠️ {shownTag === 'new' ? '请立即保存以下私钥（仅显示这一次）：' : '当前私钥（请勿泄露，保存后点「我已妥善保存」关闭）：'}
      </p>
      <div class="key-row">
        <input readonly value={shownKey} />
        <button class="ghost" on:click={() => copyText(shownKey, 'key')}>
          {copied === 'key' ? '已复制' : '复制'}
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
  .risk {
    background: #fef2f2;
    border: 1px solid #fecaca;
    color: #b91c1c;
    border-radius: 8px;
    padding: 12px 14px;
    font-size: 13px;
    line-height: 1.6;
    margin-bottom: 14px;
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
  input[type='password'] { width: 100%; }
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
  button.ghost { background: #eef2ff; color: #2563eb; }
  button.ghost:disabled { background: #eef2ff; color: #9db4e8; }
  /* 「我已妥善保存」按钮：两态都保持高对比 */
  button.ok { background: #15803d; color: #ffffff; font-weight: 600; }
  button.ok:disabled {
    background: #f0fdf4;
    color: #166534;
    border: 1px solid #86efac;
    font-weight: 600;
    cursor: default;
  }
  .export-box {
    margin-top: 14px;
    padding: 14px;
    background: #f8fafc;
    border: 1px solid #dbe3ef;
    border-radius: 8px;
  }
  .export-box p { margin: 0 0 8px; font-size: 13px; }
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
