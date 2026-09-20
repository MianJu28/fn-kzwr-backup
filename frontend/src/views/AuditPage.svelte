<script>
  // 操作审计独立页：页面自持数据（进入即加载，刷新按钮重新拉取）
  import { onMount } from 'svelte';
  import AuditSection from '../components/AuditSection.svelte';
  import { api } from '../lib/api.js';
  import { toast } from '../lib/toast.js';

  export let busy = false; // 全局操作进行中（其他页面的保存等）

  let entries = [];
  let loading = false;

  async function load() {
    loading = true;
    try {
      const d = await api.auditLog(100);
      entries = d.entries || [];
      if (d.error) toast.error(d.error);
    } catch (e) {
      toast.error(e.message, '读取审计失败');
    } finally {
      loading = false;
    }
  }

  /** 清空审计（AuditSection 弹窗内输入管理员口令） */
  async function clearAudit(passphrase) {
    try {
      return await api.auditClear(passphrase);
    } catch (e) {
      toast.error(e.message, '清空审计失败');
      return { error: e.message };
    }
  }

  // 进入页面即加载最新记录（切页由 App.go 触发重挂载）
  onMount(load);
</script>

<AuditSection {entries} busy={busy || loading} onLoad={load} onClear={clearAudit} />
