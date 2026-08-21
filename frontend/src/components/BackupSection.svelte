<script>
  // 备份执行 + 结果
  export let busy = false;
  export let backupResult = null;
  export let onRunBackup = null; // () => Promise
</script>

<section>
  <h2>⬆️ 备份</h2>
  <p class="hint">将配置的文件夹增量加密备份至 kzwr。</p>
  <button on:click={onRunBackup} disabled={busy}>
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
        {#if backupResult.orphan_removed > 0}
          <li>清理孤儿：{backupResult.orphan_removed}</li>
        {/if}
      </ul>
    </div>
  {/if}
</section>

<style>
  section {
    background: #fff;
    border-radius: 10px;
    padding: 20px;
    margin-top: 16px;
    box-shadow: 0 1px 3px rgba(0,0,0,.06);
  }
  h2 { margin: 0 0 8px; font-size: 18px; }
  .hint { color: #5a6a7a; font-size: 13px; margin: 0 0 10px; }
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
  .result { margin-top: 14px; padding: 12px; background: #ecfdf3; border-radius: 6px; }
  .result p { margin: 0 0 6px; font-weight: 600; }
  .result ul { margin: 0; padding-left: 20px; }
</style>
