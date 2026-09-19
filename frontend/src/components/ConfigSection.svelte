<script>
  // 配置导入/导出（含 WebDAV 凭据与 age 私钥；需管理员口令）
  export let busy = false;
  export let onExportConfig = null; // (passphrase) => Promise<{success, config, error}>
  export let onImportConfig = null; // (passphrase, configText) => Promise<{success, error}>

  let passphrase = '';
  let exportText = '';
  let importText = '';
  let msg = '';
  let msgOk = false;
  let working = false;
  let copied = false;

  async function doExport() {
    if (!passphrase.trim()) {
      msg = '请先输入管理员口令';
      msgOk = false;
      return;
    }
    working = true;
    msg = '';
    const r = await onExportConfig(passphrase.trim());
    working = false;
    if (r.success) {
      exportText = r.config || '';
      msg = '配置已生成：请立即妥善保存（含 WebDAV 凭据与私钥）';
      msgOk = true;
    } else {
      msg = `导出失败: ${r.error}`;
      msgOk = false;
    }
  }

  async function doImport() {
    if (!passphrase.trim()) {
      msg = '请先输入管理员口令';
      msgOk = false;
      return;
    }
    if (!importText.trim()) {
      msg = '请粘贴或选择配置文件';
      msgOk = false;
      return;
    }
    if (!confirm('导入将覆盖当前备份路径 / 通知 / WebDAV 凭据（若含私钥也会一并恢复）。确定继续？')) return;
    working = true;
    msg = '';
    const r = await onImportConfig(passphrase.trim(), importText.trim());
    working = false;
    if (r.success) {
      msg = '配置已导入并立即生效';
      msgOk = true;
      importText = '';
      exportText = '';
    } else {
      msg = `导入失败: ${r.error}`;
      msgOk = false;
    }
  }

  async function copyExport() {
    if (!exportText) return;
    try {
      if (navigator.clipboard && window.isSecureContext) {
        await navigator.clipboard.writeText(exportText);
      } else {
        const ta = document.createElement('textarea');
        ta.value = exportText;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        document.execCommand('copy');
        document.body.removeChild(ta);
      }
      copied = true;
      setTimeout(() => (copied = false), 2000);
    } catch (e) {
      msg = '复制失败，请手动选中复制';
      msgOk = false;
    }
  }

  function downloadExport() {
    if (!exportText) return;
    const blob = new Blob([exportText], { type: 'application/json' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = 'fnos-backup-config.json';
    a.click();
    URL.revokeObjectURL(a.href);
  }

  function onFile(e) {
    const f = e.target.files && e.target.files[0];
    if (!f) return;
    const reader = new FileReader();
    reader.onload = () => (importText = String(reader.result || ''));
    reader.readAsText(f);
  }
</script>

<section>
  <h2>📦 配置备份 / 恢复</h2>
  <p class="hint">
    导出后可在重装或更换设备时一键恢复：备份路径、目标文件夹、定时、通知 Webhook、WebDAV 凭据，以及 age 私钥。<strong>导出内容含敏感信息，请妥善保管。</strong>
  </p>

  <label>管理员口令（导入/导出均需校验）
    <input
      type="password"
      bind:value={passphrase}
      autocomplete="off"
      placeholder="安装时设置的管理员口令"
    />
  </label>

  <div class="btn-row">
    <button on:click={doExport} disabled={busy || working || !passphrase.trim()}>
      {working ? '处理中...' : '导出配置'}
    </button>
    <button class="ghost" on:click={copyExport} disabled={!exportText}>复制</button>
    <button class="ghost" on:click={downloadExport} disabled={!exportText}>下载 JSON</button>
  </div>

  {#if exportText}
    <textarea rows="6" readonly value={exportText}></textarea>
    {#if copied}<p class="ok">已复制到剪贴板</p>{/if}
  {/if}

  <div class="import-block">
    <h3>导入配置</h3>
    <p class="hint sub">粘贴配置 JSON，或选择之前导出的文件。</p>
    <textarea rows="4" bind:value={importText} placeholder={'{\n  "version": 1, ...\n}'}></textarea>
    <div class="btn-row">
      <label class="file-btn">
        选择文件
        <input type="file" accept=".json,application/json" on:change={onFile} />
      </label>
      <button on:click={doImport} disabled={busy || working || !importText.trim()}>
        {working ? '处理中...' : '导入配置'}
      </button>
    </div>
  </div>

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
  h3 { margin: 0 0 4px; font-size: 15px; }
  .hint {
    color: #5a6a7a;
    font-size: 13px;
    line-height: 1.6;
    margin: 0 0 14px;
  }
  .hint.sub { margin: 4px 0 8px; font-size: 12px; }
  label { display: block; margin: 4px 0 4px; font-size: 14px; color: #42526e; }
  input[type='password'],
  textarea {
    width: 100%;
    padding: 8px 10px;
    border: 1px solid #d0d7e2;
    border-radius: 6px;
    margin-top: 6px;
    font-size: 13px;
    font-family: monospace;
    box-sizing: border-box;
  }
  textarea { resize: vertical; background: #fbfcfe; }
  .btn-row { display: flex; gap: 10px; flex-wrap: wrap; margin-top: 10px; align-items: center; }
  button {
    background: #2563eb;
    color: #fff;
    border: none;
    border-radius: 6px;
    padding: 9px 16px;
    font-size: 14px;
    cursor: pointer;
    margin: 0;
  }
  button:disabled { background: #9db4e8; cursor: not-allowed; }
  button.ghost { background: #eef2ff; color: #2563eb; }
  button.ghost:disabled { background: #eef2ff; color: #9db4e8; }
  .import-block { margin-top: 16px; padding-top: 14px; border-top: 1px solid #eef1f6; }
  .file-btn {
    display: inline-block;
    background: #f1f5f9;
    color: #334155;
    border: 1px solid #e2e8f0;
    border-radius: 6px;
    padding: 9px 16px;
    font-size: 14px;
    cursor: pointer;
    margin: 0;
  }
  .file-btn input { display: none; }
  .ok { color: #22a06b; font-size: 13px; }
  .warn { color: #b45309; font-size: 13px; }
</style>
