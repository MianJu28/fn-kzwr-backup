/** 通用格式化工具（前端展示层，不含业务逻辑） */

/** 字节可读化：1024 进制，保留 1 位小数 */
export function fmtBytes(n) {
  if (n === null || n === undefined || isNaN(n)) return '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = Number(n);
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** 速率格式化（字节/秒） */
export function fmtBps(bps) {
  if (!bps || bps <= 0) return '—';
  return `${fmtBytes(bps)}/s`;
}

/** 时长格式化：m:ss / h:mm:ss */
export function fmtDuration(ms) {
  if (!ms || ms < 0) return '0s';
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (x) => String(x).padStart(2, '0');
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

/** 本地时间（日期 + 时分秒） */
export function fmtTime(ts) {
  if (!ts) return '';
  try {
    return new Date(ts).toLocaleString();
  } catch (e) {
    return '';
  }
}

/** 本地时间（仅时分秒） */
export function fmtClock(ts) {
  if (!ts) return '';
  try {
    return new Date(ts).toLocaleTimeString();
  } catch (e) {
    return '';
  }
}

/** 百分比（0-100 整数） */
export function pctOf(done, total) {
  if (!total) return 0;
  return Math.min(100, Math.round((done / total) * 100));
}

/** 千分位整数 */
export function fmtInt(n) {
  if (n === null || n === undefined || isNaN(n)) return '0';
  return Number(n).toLocaleString();
}
