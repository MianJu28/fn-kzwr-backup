<script>
  // 配置导入 / 导出（含 WebDAV 凭据与 age 私钥，需管理员口令）
  import Icon from './Icon.svelte';
  import { confirmDialog } from '../lib/confirm.js';

  export let busy = false;
  export let onExportConfig = null; // (passphrase) => Promise<{success, config, error}>
  export let onImportConfig = null; // (passphrase, configText) => Promise<{success, error}>

  let exportText = '';
  let importText = '';
  let msg = '';
  let msgOk = false;
  let working = false;
  let copied = false;
  let fileName = '';

  async function copyText(text) {
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
      copied = true;
      setTimeout(() => (copied = false), 2000);
    } catch (e) {
      msg = '复制失败，请手动选中复制';
      msgOk = false;
    }
  }

  async function doExport() {
    // 口令经统一弹窗收集（input 模式）
    const pass = await confirmDialog({
      title: '导出配置？',
      message:
        '导出内容包含 WebDAV 凭据与 age 私钥明文，需验证管理员口令。\n请勿在公共场合泄露导出结果。',
      confirmText: '验证并导出',
      danger: true,
      input: true,
      placeholder: '管理员口令',
    });
    if (pass === false || !pass) return;

    working = true;
    msg = '';
    const r = await onExportConfig(pass);
    working = false;
    if (r.success) {
      exportText = r.config || '';
      msg = '配置已生成，请立即妥善保存（含 WebDAV 凭据与私钥）';
      msgOk = true;
    } else {
      msg = `导出失败：${r.error}`;
      msgOk = false;
    }
  }

  function downloadExport() {
    if (!exportText) return;
    const blob = new Blob([exportText], { type: 'application/json' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = 'fn-kzwr-backup-config.json';
    a.click();
    URL.revokeObjectURL(a.href);
  }

  async function doImport() {
    if (!importText.trim()) {
      msg = '请粘贴或选择配置文件';
      msgOk = false;
      return;
    }
    // 口令经统一弹窗收集（input 模式）
    const pass = await confirmDialog({
      title: '导入配置？',
      message:
        '导入将覆盖当前的备份路径、目标文件夹、定时任务与通知设置；若配置包内含 age 私钥，也会一并恢复。\n请输入管理员口令以继续：',
      confirmText: '验证并导入',
      danger: true,
      input: true,
      placeholder: '管理员口令',
    });
    if (pass === false || !pass) return;

    working = true;
    msg = '';
    const r = await onImportConfig(pass, importText.trim());
    working = false;
    if (r.success) {
      msg = '配置已导入并立即生效';
      msgOk = true;
      importText = '';
      exportText = '';
      fileName = '';
    } else {
      msg = `导入失败：${r.error}`;
      msgOk = false;
    }
  }

  function onFile(e) {
    const f = e.target.files && e.target.files[0];
    if (!f) return;
    fileName = f.name;
    const reader = new FileReader();
    reader.onload = () => (importText = String(reader.result || ''));
    reader.readAsText(f);
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="package" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">配置备份 / 迁移</h2>
      <p class="card-desc">
        导出后可一键恢复：备份路径、定时任务、通知 Webhook、WebDAV 凭据与 age 私钥
        <strong>（含敏感信息，请妥善保管）</strong>
      </p>
    </div>
  </div>

  <div class="card-body">
    <p class="card-desc head-desc">导入 / 导出均需验证管理员口令（在弹窗中输入）。</p>

    <div class="divider-title">导出</div>
    <div class="row-wrap">
      <button class="btn btn-primary" on:click={doExport} disabled={busy || working}>
        {#if working}<span class="spin"></span>处理中…{:else}<Icon name="download" size={15} />导出配置{/if}
      </button>
      <button class="btn btn-ghost" on:click={() => copyText(exportText)} disabled={!exportText}>
        <Icon name={copied ? 'check' : 'copy'} size={14} />{copied ? '已复制' : '复制'}
      </button>
      <button class="btn btn-ghost" on:click={downloadExport} disabled={!exportText}>
        <Icon name="file" size={14} />下载 JSON
      </button>
    </div>

    {#if exportText}
      <textarea class="textarea mono out" rows="6" readonly value={exportText}></textarea>
    {/if}

    <div class="divider-title">导入</div>
    <p class="card-desc head-desc">粘贴配置 JSON，或选择之前导出的文件。</p>
    <textarea
      class="textarea mono"
      rows="4"
      bind:value={importText}
      placeholder={'{\n  "version": 1, ...\n}'}
    ></textarea>

    <div class="row-wrap import-actions">
      <label class="btn btn-ghost file-btn">
        <Icon name="folder-open" size={14} />选择文件
        <input type="file" accept=".json,application/json" on:change={onFile} />
      </label>
      {#if fileName}
        <span class="badge"><Icon name="file" size={11} />{fileName}</span>
      {/if}
      <button
        class="btn btn-danger"
        on:click={doImport}
        disabled={busy || working || !importText.trim()}
      >
        {#if working}<span class="spin"></span>处理中…{:else}<Icon name="refresh" size={14} />导入并覆盖{/if}
      </button>
    </div>

    {#if msg}
      <div class="alert {msgOk ? 'alert-ok' : 'alert-danger'} msg">
        <Icon name={msgOk ? 'check-circle' : 'x-circle'} size={15} />
        <div class="alert-body">{msg}</div>
      </div>
    {/if}
  </div>
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .head-desc {
    margin-bottom: var(--s2);
  }
  .out {
    margin-top: var(--s3);
    background: var(--surface-3);
  }
  .import-actions {
    margin-top: var(--s3);
  }
  .file-btn {
    cursor: pointer;
  }
  .file-btn input {
    display: none;
  }
  .msg {
    margin-top: var(--s3);
  }
</style>
