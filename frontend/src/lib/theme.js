/** 主题（浅色 / 深色）：持久化到 localStorage，跟随系统偏好作为初值 */
import { writable } from 'svelte/store';

const KEY = 'fn-kzwr-backup.theme';

/** 当前主题：'light' | 'dark' */
export const theme = writable('light');

function apply(t) {
  document.documentElement.setAttribute('data-theme', t);
}

function detect() {
  try {
    const saved = localStorage.getItem(KEY);
    if (saved === 'light' || saved === 'dark') return saved;
    return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches
      ? 'dark'
      : 'light';
  } catch (e) {
    return 'light';
  }
}

/** 应用启动时调用一次 */
export function initTheme() {
  const t = detect();
  apply(t);
  theme.set(t);
}

export function toggleTheme() {
  theme.update((cur) => {
    const next = cur === 'dark' ? 'light' : 'dark';
    apply(next);
    try {
      localStorage.setItem(KEY, next);
    } catch (e) {
      /* 忽略隐私模式下的写入失败 */
    }
    return next;
  });
}
