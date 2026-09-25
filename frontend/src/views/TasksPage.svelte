<script>
  /**
   * 任务管理：备份任务列表 + 编辑
   *
   * 每个任务 = 源文件夹 + 目标 + 定时 + 保留策略；
   * 任务之间完全独立（各自增量、各自快照、各自保留策略），可指向同一个目标。
   */
  import Icon from '../components/Icon.svelte';
  import { api } from '../lib/api.js';
  import { toast } from '../lib/toast.js';
  import { confirmDialog } from '../lib/confirm.js';
  import { pickBackupFolder, isInTrimHost } from '../trimSdk.js';

  /** 任务列表（/api/tasks） */
  export let tasks = [];
  /** 可选目标（供下拉选择） */
  export let targetOptions = [];
  export let busy = false;
  export let onChanged = null; // () => Promise

  const CRON_PRESETS = [
    { label: '每天 00:00', value: '0 0 * * *' },
    { label: '每天 03:00', value: '0 3 * * *' },
    { label: '每小时', value: '0 * * * *' },
    { label: '每周一 02:00', value: '0 2 * * 1' },
  ];

  let editing = null;
  let saving = false;
  let running = null; // 正在运行的任务 id
  let pathInput = '';
  let previewNext = [];
  let previewErr = '';
  let inTrimHost = isInTrimHost();

  function blankTask() {
    return {
      id: '',
      name: '',
      enabled: true,
      paths: [],
      target_id: targetOptions.length ? targetOptions[0].id : '',
      target_folder: 'fn-backup',
      schedule_cron: '',
      retention: {
        enabled: false,
        cleanup_unmanaged: false,
        min_age_days: 0,
        empty_recycle_bin: false,
        recycle_max_gb: 0,
        recycle_min_age_days: 0,
      },
    };
  }

  function startCreate() {
    editing = blankTask();
    previewNext = [];
    previewErr = '';
    pathInput = '';
  }

  function startEdit(t) {
    editing = {
      id: t.id,
      name: t.name || '',
      enabled: t.enabled !== false,
      paths: [...(t.paths || [])],
      target_id: t.target_id || (targetOptions.length ? targetOptions[0].id : ''),
      target_folder: t.target_folder || 'fn-backup',
      schedule_cron: t.schedule_cron || '',
      retention: { ...blankTask().retention, ...(t.retention || {}) },
    };
    previewNext = t.schedule_next || [];
    previewErr = '';
    pathInput = '';
  }

  function cancelEdit() {
    editing = null;
    previewNext = [];
    previewErr = '';
  }

  function setField(k, v) {
    editing = { ...editing, [k]: v };
  }

  function setRetention(k, v) {
    editing = { ...editing, retention: { ...editing.retention, [k]: v } };
  }

  function addPath(p) {
    const v = (p || '').trim();
    if (!v) return;
    if (editing.paths.includes(v)) {
      toast.info('该路径已在列表中');
      return;
    }
    setField('paths', [...editing.paths, v]);
  }

  function removePath(i) {
    setField(
      'paths',
      editing.paths.filter((_, idx) => idx !== i)
    );
  }

  async function pickDirs() {
    try {
      const dirs = await pickBackupFolder();
      if (!dirs || dirs.length === 0) {
        toast.info('已取消选择');
        return;
      }
      let added = 0;
      let next = [...editing.paths];
      for (const d of dirs) {
        const p = d.trim();
        if (p && !next.includes(p)) {
          next.push(p);
          added += 1;
        }
      }
      setField('paths', next);
      toast.success(`已添加 ${added} 个目录`);
    } catch (e) {
      toast.error(e.message || '选择目录失败');
    }
  }

  async function previewCron(cron) {
    previewErr = '';
    previewNext = [];
    if (!cron.trim()) return;
    try {
      const r = await api.schedulePreview(cron.trim());
      if (r && r.valid) previewNext = r.next || [];
      else previewErr = (r && r.error) || '表达式无效';
    } catch (e) {
      previewErr = e.message;
    }
  }

  async function save() {
    if (!editing) return;
    if (!editing.name.trim()) {
      toast.error('请填写任务名称');
      return;
    }
    if (editing.paths.length === 0) {
      toast.error('请至少添加一个源文件夹');
      return;
    }
    if (!editing.target_id) {
      toast.error('请先在「目标管理」创建目标');
      return;
    }
    saving = true;
    try {
      const body = {
        name: editing.name.trim(),
        enabled: !!editing.enabled,
        paths: editing.paths,
        target_id: editing.target_id,
        target_folder: editing.target_folder.trim() || 'fn-backup',
        schedule_cron: editing.schedule_cron,
        retention: editing.retention,
      };
      if (editing.id) body.id = editing.id;
      const r = await api.saveTask(body);
      if (r.error) {
        toast.error(r.error);
        return;
      }
      toast.success(editing.id ? '任务已更新' : '任务已创建');
      editing = null;
      if (onChanged) await onChanged();
    } catch (e) {
      toast.error(e.message);
    } finally {
      saving = false;
    }
  }

  async function run(t) {
    running = t.id;
    try {
      const r = await api.runTask(t.id);
      if (r.error) {
        toast.error(r.error, `「${t.name || t.id}」备份失败`);
      } else if (r.skipped) {
        toast.warn(r.error || '已有备份在执行，本次已跳过');
      } else {
        toast.success(`上传 ${r.uploaded} 个文件（${fmtBytes(r.uploaded_bytes || 0)}）`, `「${t.name || t.id}」备份完成`);
      }
      if (onChanged) await onChanged();
    } catch (e) {
      toast.error(e.message);
    } finally {
      running = null;
    }
  }

  async function toggleEnabled(t) {
    try {
      const r = await api.saveTask({ id: t.id, enabled: !t.enabled });
      if (r.error) toast.error(r.error);
      else toast.success(t.enabled ? '任务已停用' : '任务已启用');
      if (onChanged) await onChanged();
    } catch (e) {
      toast.error(e.message);
    }
  }

  async function remove(t) {
    const yes = await confirmDialog({
      title: '删除任务？',
      message: `将删除任务「${t.name || t.id}」。云端已备份的文件不会被删除，但它的增量快照记录会保留（可选一并清理）。`,
      confirmText: '删除任务',
      danger: true,
    });
    if (!yes) return;
    const purge = await confirmDialog({
      title: '同时清理快照记录？',
      message:
        '清理后该任务下次备份会重新上传全部文件（因为不再有差分基线）。如果只是要重命名任务，选「保留」。',
      confirmText: '清理记录',
      cancelText: '保留记录',
      danger: true,
    });
    try {
      const r = await api.deleteTask(t.id, !!purge);
      if (r.error) {
        toast.error(r.error);
        return;
      }
      toast.success(purge ? `任务已删除，清理快照 ${r.purged || 0} 条` : '任务已删除');
      if (onChanged) await onChanged();
    } catch (e) {
      toast.error(e.message);
    }
  }

  function fmtBytes(n) {
    if (!n) return '0 B';
    const u = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0;
    let v = Number(n);
    while (v >= 1024 && i < u.length - 1) {
      v /= 1024;
      i += 1;
    }
    return `${v.toFixed(v >= 10 || i === 0 ? 0 : 1)} ${u[i]}`;
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="package" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">备份任务</h2>
      <p class="card-desc">
        每个任务 = 源文件夹 + 目标 + 定时 + 保留策略，各自独立增量与快照；
        同一个源可以出现在多个任务里（分别备份到不同目标）
      </p>
    </div>
    <span class="badge">{tasks.length} 个任务</span>
  </div>

  <div class="card-body">
    {#if tasks.length === 0}
      <div class="empty">
        <Icon name="upload" size={22} />
        <p>还没有备份任务，新建一个即可开始增量备份</p>
      </div>
    {/if}

    {#each tasks as t (t.id)}
      <div class="row-item">
        <div class="grow">
          <div class="row-title">
            {t.name || t.id}
            {#if !t.enabled}
              <span class="badge badge-warn">已停用</span>
            {:else if !t.target_ready}
              <span class="badge badge-danger">目标未就绪</span>
            {:else}
              <span class="badge badge-ok">已启用</span>
            {/if}
          </div>
          <div class="row-sub">
            <span class="meta"><b>源</b>{t.paths ? t.paths.length : 0} 个</span>
            <span class="meta"><b>目标</b>{t.target_name || t.target_id}</span>
            <span class="meta"><b>目录</b>{t.target_folder}</span>
            {#if t.schedule_cron}
              <span class="meta"><b>定时</b>{t.schedule_cron}
                {#if t.schedule_next && t.schedule_next.length}（下次 {t.schedule_next[0]}）{/if}
              </span>
            {:else}
              <span class="meta"><b>定时</b>未启用</span>
            {/if}
            {#if t.retention && t.retention.enabled}
              <span class="meta"><b>保留</b>{t.retention.cleanup_unmanaged ? '清理孤儿' : '仅记录'}</span>
            {/if}
          </div>
          <div class="row-sub">
            {#each t.paths || [] as p}
              <code>{p}</code>
            {/each}
          </div>
        </div>
        <div class="row-actions">
          <button class="btn btn-sm btn-primary" on:click={() => run(t)}
            disabled={busy || running === t.id || !t.enabled}>
            {running === t.id ? '备份中…' : '立即备份'}
          </button>
          <button class="btn btn-sm" on:click={() => toggleEnabled(t)} disabled={busy}>
            {t.enabled ? '停用' : '启用'}
          </button>
          <button class="btn btn-sm" on:click={() => startEdit(t)} disabled={busy}>编辑</button>
          <button class="btn btn-sm btn-danger" on:click={() => remove(t)} disabled={busy}>删除</button>
        </div>
      </div>
    {/each}

    {#if editing}
      <div class="editor">
        <div class="field">
          <label for="tk-name">任务名称</label>
          <input id="tk-name" placeholder="如：文档备份" value={editing.name}
            on:input={(e) => setField('name', e.target.value)} disabled={saving} />
        </div>

        <div class="field">
          <label for="tk-path">源文件夹（可多选）</label>
          <div class="field-row">
            <input id="tk-path" placeholder="手动输入绝对路径，如 /vol1/1000/Documents" value={pathInput}
              on:input={(e) => (pathInput = e.target.value)}
              on:keydown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  addPath(pathInput);
                  pathInput = '';
                }
              }}
              disabled={saving} />
            <button class="btn" on:click={() => { addPath(pathInput); pathInput = ''; }}
              disabled={saving}>添加</button>
            {#if inTrimHost}
              <button class="btn" on:click={pickDirs} disabled={saving}>选择目录</button>
            {/if}
          </div>
          {#if editing.paths.length === 0}
            <p class="field-hint">尚未添加任何文件夹</p>
          {/if}
        </div>

        {#each editing.paths as p, i}
          <div class="row-sub">
            <code>{p}</code>
            <button class="btn btn-sm btn-ghost" on:click={() => removePath(i)} disabled={saving}>移除</button>
          </div>
        {/each}

        <div class="field">
          <label for="tk-target">目标</label>
          {#if targetOptions.length === 0}
            <p class="field-error">还没有可用目标，请先到「目标管理」创建一个</p>
          {:else}
            <select id="tk-target" value={editing.target_id}
              on:change={(e) => setField('target_id', e.target.value)} disabled={saving}>
              {#each targetOptions as o}
                <option value={o.id}>{o.name || o.id}{o.ready ? '' : '（未配置凭据）'}</option>
              {/each}
            </select>
          {/if}
        </div>

        <div class="field">
          <label for="tk-folder">目标目录</label>
          <input id="tk-folder" placeholder="fn-backup" value={editing.target_folder}
            on:input={(e) => setField('target_folder', e.target.value)} disabled={saving} />
          <p class="field-hint">目标端的存放前缀；不同任务可用不同目录区分</p>
        </div>

        <div class="field">
          <label for="tk-cron">定时（cron，留空 = 仅手动）</label>
          <div class="field-row">
            <input id="tk-cron" placeholder="0 3 * * *" value={editing.schedule_cron}
              on:input={(e) => setField('schedule_cron', e.target.value)}
              on:blur={() => previewCron(editing.schedule_cron)} disabled={saving} />
          </div>
          <div class="row-sub">
            {#each CRON_PRESETS as p}
              <button class="chip" on:click={() => { setField('schedule_cron', p.value); previewCron(p.value); }}
                disabled={saving}>{p.label}</button>
            {/each}
          </div>
          {#if previewErr}
            <p class="field-error">{previewErr}</p>
          {:else if previewNext.length}
            <p class="field-hint">接下来：{previewNext.join(' · ')}</p>
          {/if}
        </div>

        <div class="field">
          <p class="field-hint">保留策略（清理目标端孤儿文件）</p>
          <label class="switch-row">
            <input type="checkbox" checked={!!editing.retention.enabled}
              on:change={(e) => setRetention('enabled', e.target.checked)} disabled={saving} />
            <span>启用保留策略</span>
          </label>
          <label class="switch-row">
            <input type="checkbox" checked={!!editing.retention.cleanup_unmanaged}
              on:change={(e) => setRetention('cleanup_unmanaged', e.target.checked)} disabled={saving} />
            <span>清理不在任何快照中的孤儿文件</span>
          </label>
          <div class="field-row">
            <label for="tk-age">只清理早于</label>
            <input id="tk-age" type="number" min="0" value={editing.retention.min_age_days}
              on:input={(e) => setRetention('min_age_days', Number(e.target.value) || 0)} disabled={saving} />
            <span class="field-hint">天（0 = 不限制）</span>
          </div>
        </div>

        <label class="switch-row">
          <input type="checkbox" checked={!!editing.enabled}
            on:change={(e) => setField('enabled', e.target.checked)} disabled={saving} />
          <span>启用该任务</span>
        </label>

        <div class="row-actions">
          <button class="btn btn-primary" on:click={save} disabled={saving}>
            {saving ? '保存中…' : editing.id ? '保存修改' : '创建任务'}
          </button>
          <button class="btn btn-ghost" on:click={cancelEdit} disabled={saving}>取消</button>
        </div>
      </div>
    {:else}
      <div class="row-actions">
        <button class="btn btn-primary" on:click={startCreate} disabled={busy}>
          <Icon name="plus" size={14} /> 新建任务
        </button>
      </div>
    {/if}
  </div>
</section>
