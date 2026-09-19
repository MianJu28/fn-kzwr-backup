<script>
  // age 密钥管理（ADR-003）：查看公钥 / 自定义私钥 / 自动生成 / 显示并另存 / 备份确认
  import Icon from './Icon.svelte';
  import { confirmDialog } from '../lib/confirm.js';
  import { toast } from '../lib/toast.js';

  export let keyInfo = null; // { public_key }
  export let busy = false;
  export let revealKey = ''; // 首次启动自动生成的私钥（一次性下发）
  export let backedUp = false;
  export let onSetKey = null; // (privateKey) => Promise<{success, public_key, error}>
  export let onGenerateKey = null; // () => Promise<{success, private_key, public_key, error}>
  export let onExportKey = null; // (passphrase) => Promise<{private_key, error}>
  export let onBackupAck = null; // () => Promise<void>

  let privateKeyInput = '';
  let adminPassphrase = '';
  let showExportBox = false;
  let showPrivateInput = false;
  let shownKey = revealKey || '';
  let shownTag = revealKey ? 'new' : ''; // new=新生成 / export=导出的当前私钥
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
      toast.error('复制失败，请手动选中复制');
    }
  }

  async function saveCustom() {
    if (!privateKeyInput.trim()) return;
    const yes = await confirmDialog({
      title: '使用此私钥？',
      message: '更换私钥后，此前用旧公钥加密的备份数据将无法解密恢复。',
      confirmText: '确认更换',
      danger: true,
    });
    if (!yes) return;

    working = true;
    const r = await onSetKey(privateKeyInput.trim());
    working = false;
    if (r.success) {
      privateKeyInput = '';
      shownKey = '';
      shownTag = '';
      showPrivateInput = false;
      toast.success('私钥已更新并立即生效（由你提供，已标记为已备份）');
    } else {
      toast.error(`设置失败：${r.error}`);
    }
  }

  async function generate() {
    const yes = await confirmDialog({
      title: '生成全新密钥对？',
      message: '将替换当前密钥。此前备份的数据将无法解密恢复。',
      confirmText: '生成并替换',
      danger: true,
    });
    if (!yes) return;

    working = true;
    const r = await onGenerateKey();
    working = false;
    if (r.success) {
      shownKey = r.private_key;
      shownTag = 'new';
      toast.warn('已生成新密钥对，请立即保存下方私钥', '密钥已轮换');
    } else {
      toast.error(`生成失败：${r.error}`);
    }
  }

  function startExport() {
    showExportBox = true;
    showPrivateInput = false;
  }

  function cancelExport() {
    showExportBox = false;
    adminPassphrase = '';
  }

  async function exportKey() {
    if (!adminPassphrase.trim()) {
      toast.warn('请输入管理员口令');
      return;
    }
    const yes = await confirmDialog({
      title: '显示私钥明文？',
      message: '请勿在公共场所或截图中泄露。确认后私钥将显示在页面上。',
      confirmText: '确认显示',
      danger: true,
    });
    if (!yes) return;

    working = true;
    const r = await onExportKey(adminPassphrase.trim());
    working = false;
    if (r.private_key) {
      shownKey = r.private_key;
      shownTag = 'export';
      showExportBox = false;
      adminPassphrase = '';
      toast.info('已显示当前私钥，请妥善保存到安全位置');
    } else {
      toast.error(`导出失败：${r.error}`);
    }
  }

  async function ackBackup() {
    working = true;
    await onBackupAck();
    working = false;
    shownKey = '';
    shownTag = '';
    showExportBox = false;
    adminPassphrase = '';
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap {backedUp ? 'ok' : 'danger'}"><Icon name="key" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">加密密钥</h2>
      <p class="card-desc">
        备份用 age 公钥加密，恢复需对应私钥。私钥经管理员口令加密存储在本机，
        <strong>请务必另行备份</strong>
      </p>
    </div>
    <span class="badge {backedUp ? 'badge-ok' : 'badge-danger'}">
      <Icon name={backedUp ? 'check-circle' : 'alert'} size={12} />
      {backedUp ? '已备份' : '未备份'}
    </span>
  </div>

  <div class="card-body">
    {#if !backedUp}
      <div class="alert alert-danger">
        <Icon name="alert" size={16} />
        <div class="alert-body">
          <div class="alert-title">私钥尚未确认备份</div>
          私钥一旦丢失，已备份的数据将永久无法恢复。请点击下方「显示私钥」保存到安全位置，再点「我已妥善保存」。
        </div>
      </div>
    {/if}

    <label class="field current">
      <span class="label">当前公钥</span>
      <div class="key-row">
        <input class="input mono" readonly value={keyInfo?.public_key || ''} />
        <button class="btn btn-ghost" on:click={() => copyText(keyInfo?.public_key || '', 'pub')}>
          <Icon name={copied === 'pub' ? 'check' : 'copy'} size={14} />
          {copied === 'pub' ? '已复制' : '复制'}
        </button>
      </div>
      <span class="field-hint">此公钥用于加密上传，可安全公开</span>
    </label>
  </div>

  <div class="card-foot actions">
    <button class="btn btn-soft" on:click={startExport} disabled={busy || working}>
      <Icon name="eye" size={15} />显示私钥
    </button>
    <button
      class="btn btn-success"
      on:click={ackBackup}
      disabled={busy || working || backedUp}
      title={backedUp ? '已确认备份' : '确认已把私钥保存到安全位置'}
    >
      <Icon name="check-circle" size={15} />
      {backedUp ? '已妥善保存' : '我已妥善保存'}
    </button>
    <span class="spacer"></span>
    <button class="btn btn-ghost" on:click={() => (showPrivateInput = !showPrivateInput)}>
      <Icon name="key" size={14} />{showPrivateInput ? '收起' : '更换密钥'}
    </button>
  </div>

  {#if showExportBox}
    <div class="card-body block">
      <div class="alert alert-warn">
        <Icon name="lock" size={15} />
        <div class="alert-body">显示私钥需要先验证管理员口令（安装应用时设置）</div>
      </div>
      <label class="field">
        <span class="label">管理员口令</span>
        <input
          class="input"
          type="password"
          bind:value={adminPassphrase}
          autocomplete="off"
          placeholder="安装时设置的管理员口令"
        />
      </label>
      <div class="row-wrap">
        <button class="btn btn-primary" on:click={exportKey} disabled={working || !adminPassphrase.trim()}>
          {#if working}<span class="spin"></span>校验中…{:else}<Icon name="eye" size={15} />确认显示{/if}
        </button>
        <button class="btn btn-ghost" on:click={cancelExport} disabled={working}>取消</button>
      </div>
    </div>
  {/if}

  {#if shownKey}
    <div class="card-body block">
      <div class="alert {shownTag === 'new' ? 'alert-warn' : 'alert-info'}">
        <Icon name="alert" size={15} />
        <div class="alert-body">
          {shownTag === 'new'
            ? '请立即保存以下私钥（仅显示这一次）'
            : '当前私钥（请勿泄露；保存后点击「我已妥善保存」关闭提示）'}
        </div>
      </div>
      <div class="key-row">
        <textarea class="textarea mono" rows="2" readonly value={shownKey}></textarea>
        <button class="btn btn-ghost" on:click={() => copyText(shownKey, 'key')}>
          <Icon name={copied === 'key' ? 'check' : 'copy'} size={14} />
          {copied === 'key' ? '已复制' : '复制'}
        </button>
      </div>
    </div>
  {/if}

  {#if showPrivateInput}
    <div class="card-body block">
      <div class="divider-title">使用自定义私钥</div>
      <label class="field">
        <span class="label">
          私钥 <span class="opt">（AGE-SECRET-KEY-1…，留空则沿用当前密钥）</span>
        </span>
        <textarea
          class="textarea mono"
          rows="2"
          bind:value={privateKeyInput}
          placeholder="AGE-SECRET-KEY-1..."
        ></textarea>
      </label>
      <div class="row-wrap">
        <button
          class="btn btn-primary"
          on:click={saveCustom}
          disabled={busy || working || !privateKeyInput.trim()}
        >
          <Icon name="check" size={15} />使用此私钥
        </button>
        <button class="btn btn-ghost" on:click={generate} disabled={busy || working}>
          <Icon name="refresh" size={15} />自动生成新密钥对
        </button>
      </div>
    </div>
  {/if}
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .current {
    margin-top: var(--s3);
  }
  .key-row {
    display: flex;
    gap: var(--s2);
    align-items: flex-start;
  }
  .key-row .input,
  .key-row .textarea {
    flex: 1;
    min-width: 0;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: var(--s2);
    flex-wrap: wrap;
  }
  .block {
    padding-top: 0;
    display: flex;
    flex-direction: column;
    gap: var(--s3);
  }
  .block .divider-title {
    margin: 0;
  }
</style>
