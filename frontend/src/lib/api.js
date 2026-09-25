/**
 * 后端 API 客户端（所有 HTTP 调用集中于此，组件不直接拼 URL）
 *
 * 约定：后端统一返回 200 + JSON（错误放在 `error` 字段），
 * 因此这里只做网络层异常抛出与 JSON 解析，业务错误由调用方判断。
 *
 * 路径统一加网关前缀（APP_BASE）：经飞牛统一网关访问时，应用在
 * `/app/<appname>` 下，裸 `/api` 会落到宿主服务上。
 */
import { APP_BASE } from './appBase.js';

async function req(path, options = {}) {
  const res = await fetch(APP_BASE + path, options);
  let data;
  try {
    data = await res.json();
  } catch (e) {
    throw new Error(`响应解析失败（HTTP ${res.status}）`);
  }
  if (!res.ok && data && data.error === undefined) {
    throw new Error(`请求失败（HTTP ${res.status}）`);
  }
  return data;
}

const get = (path) => req(path);

const post = (path, body) =>
  req(path, {
    method: 'POST',
    headers: body === undefined ? undefined : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });

export const api = {
  // 健康检查
  health: () => get('/api/health'),

  // 配置（备份路径 / 目标文件夹 / 定时）
  config: () => get('/api/config'),
  saveConfig: (cfg) => post('/api/config', cfg),

  // WebDAV 凭据（地址固定，后端保存前实测连通性）
  saveWebdav: (username, password) => post('/api/webdav/config', { username, password }),
  userInfo: () => get('/api/user/info'),

  // age 密钥
  keys: () => get('/api/keys'),
  setKey: (private_key) => post('/api/keys', { private_key }),
  generateKey: () => post('/api/keys/generate'),
  exportKey: (passphrase) => post('/api/keys/export', { passphrase }),
  backupAck: () => post('/api/keys/backup-ack'),

  // 告警与通知
  alerts: () => get('/api/alerts'),
  clearAlerts: () => req('/api/alerts', { method: 'DELETE' }),
  saveWebhook: (webhook_url, headers, body_template) =>
    post('/api/notify/webhook', { webhook_url, headers, body_template }),
  testWebhook: (webhook_url, headers, body_template) =>
    post('/api/notify/webhook/test', { webhook_url, headers, body_template }),

  // 配置导入 / 导出（需管理员口令）
  exportConfig: (passphrase) => post('/api/config/export', { passphrase }),
  importConfig: (passphrase, config) => post('/api/config/import', { passphrase, config }),

  // 备份 / 恢复
  runBackup: () => post('/api/backup/run'),
  /** 备份文件夹概况（文件数/文件夹数/总大小） */
  restoreFiles: () => get('/api/restore/files'),
  /** 按目录懒加载：只取一层子项（目录附递归统计） */
  restoreTree: (source, dir = '') =>
    get(`/api/restore/tree?source=${encodeURIComponent(source)}&dir=${encodeURIComponent(dir)}`),
  /** 恢复：all=true 时恢复该源路径（可用 dir 限定子目录）下的全部文件 */
  restore: (files, source_path, all = false, dir = '') =>
    post('/api/restore/run', { files, source_path, all, dir }),
  /** 清理快照中云端已不存在的文件记录（只动快照，不删云端文件） */
  pruneMissing: (source_path) => post('/api/restore/prune', { source_path }),

  // ── 插件（插件自带路由统一挂在 /api/p/<插件id> 下）──────────────
  /** 插件清单（内置插件 id/名称/类别/能力） */
  plugins: () => get('/api/plugins'),

  // kzwr 增强插件（非备份通道，可选，需 access-token）
  /** 账号信息（存储空间/套餐；未配置 token 时返回 configured:false + 指引） */
  kzwrUser: () => get('/api/p/kzwr/user'),
  /** 保存或清除 access-token（空串 = 清除） */
  kzwrSaveToken: (access_token) => post('/api/p/kzwr/token', { access_token }),
  /** 清空云端回收站（物理删除，不可恢复） */
  kzwrTrashEmpty: () => post('/api/p/kzwr/trash/empty'),

  /** 定时任务预览：cron → 未来 5 次触发时间（服务器本地时区） */
  schedulePreview: (cron) => post('/api/schedule/preview', { cron }),
  /** 一键体检：逐项检查配置与连通性，返回可操作建议 */
  setupCheck: () => get('/api/setup/check'),
  /** 操作审计日志（最新在前） */
  auditLog: (limit = 100) => get(`/api/audit?limit=${limit}`),
  /** 清空审计日志（需管理员口令） */
  auditClear: (passphrase) => post('/api/audit/clear', { passphrase }),
  /** 运行日志末尾（默认 800 行） */
  logsTail: (tail = 800) => get(`/api/logs?tail=${tail}`),
  /** 清空运行日志 */
  logsClear: () => post('/api/logs/clear'),
  /** 运行日志下载地址（直接 <a>/window.open） */
  logsDownloadUrl: `${APP_BASE}/api/logs/download`,
};
