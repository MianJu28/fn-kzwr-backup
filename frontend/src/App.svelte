<script>
  let health = '检查中...';
  let backupResult = null;
  let restoreResult = null;
  let busy = false;
  let error = null;

  // 恢复表单
  let restoreDir = '/volume1/restore';
  let restoreFiles = '';
  let restoreType = 'full'; // full | selective

  async function checkHealth() {
    try {
      const res = await fetch('/api/health');
      const data = await res.json();
      health = `服务正常 (v${data.version})`;
    } catch (e) {
      health = `服务异常: ${e.message}`;
    }
  }

  async function runBackup() {
    busy = true;
    error = null;
    backupResult = null;
    try {
      const res = await fetch('/api/backup/run', { method: 'POST' });
      const data = await res.json();
      backupResult = data;
      if (data.error) error = data.error;
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }

  async function runRestore() {
    busy = true;
    error = null;
    restoreResult = null;
    try {
      const body = { restore_dir: restoreDir };
      if (restoreType === 'selective' && restoreFiles.trim()) {
        body.files = restoreFiles.split('\n').map((f) => f.trim()).filter(Boolean);
      }
      const res = await fetch('/api/restore/run', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      const data = await res.json();
      restoreResult = data;
      if (data.error) error = data.error;
    } catch (e) {
      error = e.message;
    } finally {
      busy = false;
    }
  }

  checkHealth();
</script>

<main>
  <header>
    <h1>🛡️ fnos 增量加密备份</h1>
    <p class="health" class:ok={health.startsWith('服务正常')}>{health}</p>
  </header>

  {#if error}
    <div class="error">⚠️ {error}</div>
  {/if}

  <section>
    <h2>备份</h2>
    <p>将本地目录增量加密备份至酷族网软（fn-backup）。</p>
    <button on:click={runBackup} disabled={busy}>
      {busy ? '执行中...' : '立即备份'}
    </button>
    {#if backupResult && !backupResult.error}
      <div class="result">
        <p>✅ 备份完成</p>
        <ul>
          <li>上传文件：{backupResult.uploaded}</li>
          <li>上传字节：{backupResult.uploaded_bytes}</li>
          <li>删除：{backupResult.deleted}</li>
          <li>未变化：{backupResult.unchanged}</li>
        </ul>
      </div>
    {/if}
  </section>

  <section>
    <h2>恢复</h2>
    <p>从酷族网软下载解密，恢复到本地目录。</p>

    <label>恢复目标目录
      <input bind:value={restoreDir} />
    </label>

    <label class="radio">
      <input type="radio" value="full" bind:group={restoreType} />
      全量恢复
    </label>
    <label class="radio">
      <input type="radio" value="selective" bind:group={restoreType} />
      选择性恢复
    </label>

    {#if restoreType === 'selective'}
      <label>要恢复的文件（每行一个相对路径）
        <textarea bind:value={restoreFiles} rows="4" placeholder="file1.txt&#10;docs/note.md"></textarea>
      </label>
    {/if}

    <button on:click={runRestore} disabled={busy}>
      {busy ? '执行中...' : '开始恢复'}
    </button>

    {#if restoreResult && !restoreResult.error}
      <div class="result">
        <p>✅ 恢复完成</p>
        <ul>
          <li>恢复文件：{restoreResult.restored}</li>
          <li>恢复字节：{restoreResult.restored_bytes}</li>
        </ul>
      </div>
    {/if}
  </section>

  <footer>fnos-backup · age 加密 · kzwr 增量备份</footer>
</main>

<style>
  :global(body) {
    margin: 0;
    font-family: system-ui, -apple-system, sans-serif;
    background: #f5f6fa;
    color: #1f2d3d;
  }
  main {
    max-width: 720px;
    margin: 0 auto;
    padding: 24px 16px 40px;
  }
  header {
    padding: 24px 0 16px;
    border-bottom: 1px solid #e0e4ea;
  }
  h1 { margin: 0; font-size: 24px; }
  .health { color: #5a6a7a; }
  .health.ok { color: #22a06b; }
  section {
    background: #fff;
    border-radius: 10px;
    padding: 20px;
    margin-top: 16px;
    box-shadow: 0 1px 3px rgba(0,0,0,.06);
  }
  h2 { margin: 0 0 8px; font-size: 18px; }
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
  input, textarea {
    width: 100%;
    padding: 8px 10px;
    border: 1px solid #d0d7e2;
    border-radius: 6px;
    margin-top: 4px;
    font-size: 14px;
    box-sizing: border-box;
  }
  .radio { display: inline-flex; align-items: center; gap: 6px; margin-right: 16px; }
  .radio input { width: auto; }
  .result {
    margin-top: 14px;
    padding: 12px;
    background: #ecfdf3;
    border-radius: 6px;
  }
  .result p { margin: 0 0 6px; font-weight: 600; }
  .result ul { margin: 0; padding-left: 20px; }
  .error {
    background: #fef2f2;
    color: #b91c1c;
    padding: 12px;
    border-radius: 6px;
    margin-top: 12px;
  }
  footer {
    text-align: center;
    color: #8a94a6;
    font-size: 13px;
    margin-top: 28px;
  }
</style>
