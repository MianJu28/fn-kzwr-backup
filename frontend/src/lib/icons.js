/**
 * 图标库（内联 SVG，24×24 线性风格）
 *
 * 取值均为 SVG 内部标记字符串，由 Icon.svelte 通过 {@html} 渲染。
 * 统一 stroke=currentColor，故颜色由父级 CSS 控制。
 */
export const ICONS = {
  shield: '<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/>',
  grid: '<rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/>',
  upload:
    '<path d="M21 15v3a3 3 0 0 1-3 3H6a3 3 0 0 1-3-3v-3"/><path d="m7.5 8.5 4.5-4.5 4.5 4.5"/><path d="M12 4v12"/>',
  download:
    '<path d="M21 15v3a3 3 0 0 1-3 3H6a3 3 0 0 1-3-3v-3"/><path d="m7.5 11.5 4.5 4.5 4.5-4.5"/><path d="M12 16V4"/>',
  sliders:
    '<path d="M4 21v-6M4 11V3M12 21v-9M12 8V3M20 21v-4M20 13V3"/><path d="M1.5 15h5M9.5 8h5M17.5 17h5"/>',
  lock: '<rect x="3.5" y="10.5" width="17" height="10.5" rx="2.5"/><path d="M7.5 10.5V7a4.5 4.5 0 0 1 9 0v3.5"/>',
  unlock: '<rect x="3.5" y="10.5" width="17" height="10.5" rx="2.5"/><path d="M7.5 10.5V7a4.5 4.5 0 0 1 8.6-1.9"/>',
  key: '<circle cx="7.5" cy="15.5" r="3.5"/><path d="m10 13 9-9"/><path d="m16.5 6.5 2 2L21 6l-2-2"/>',
  bell: '<path d="M18 8.5a6 6 0 1 0-12 0c0 6.5-2.5 8-2.5 8h17S18 15 18 8.5z"/><path d="M13.7 20.5a2 2 0 0 1-3.4 0"/>',
  database:
    '<ellipse cx="12" cy="5.5" rx="8" ry="3"/><path d="M4 5.5v13c0 1.66 3.58 3 8 3s8-1.34 8-3v-13"/><path d="M4 12c0 1.66 3.58 3 8 3s8-1.34 8-3"/>',
  server:
    '<rect x="2.5" y="3" width="19" height="7" rx="2"/><rect x="2.5" y="14" width="19" height="7" rx="2"/><path d="M6.5 6.5h.01M6.5 17.5h.01"/>',
  clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7.5V12l3.2 2"/>',
  zap: '<path d="M13 2 4 14h7l-1 8 9-12h-7l1-8z"/>',
  check: '<path d="M20 6.5 9.5 17 4 11.5"/>',
  'check-circle': '<circle cx="12" cy="12" r="9"/><path d="m8.5 12.5 2.5 2.5 4.5-5"/>',
  x: '<path d="M18 6 6 18M6 6l12 12"/>',
  'x-circle': '<circle cx="12" cy="12" r="9"/><path d="m15 9-6 6M9 9l6 6"/>',
  alert:
    '<path d="M10.3 3.9 2.2 18a2 2 0 0 0 1.7 3h16.2a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z"/><path d="M12 9.5v4M12 17.5h.01"/>',
  info: '<circle cx="12" cy="12" r="9"/><path d="M12 16.5V12M12 8h.01"/>',
  plus: '<path d="M12 5.5v13M5.5 12h13"/>',
  minus: '<path d="M5.5 12h13"/>',
  trash:
    '<path d="M3.5 6.5h17"/><path d="M19 6.5V19a2.5 2.5 0 0 1-2.5 2.5h-9A2.5 2.5 0 0 1 5 19V6.5"/><path d="M9 6.5V4.8A2 2 0 0 1 11 3h2a2 2 0 0 1 2 2v1.7"/>',
  copy: '<rect x="8.5" y="8.5" width="12" height="12" rx="2.5"/><path d="M15.5 5.5v-1a2 2 0 0 0-2-2h-8a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h1"/>',
  eye: '<path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7-10-7-10-7z"/><circle cx="12" cy="12" r="3"/>',
  'eye-off':
    '<path d="M9.9 4.6A9.6 9.6 0 0 1 12 4.4c6.4 0 10 7 10 7a17 17 0 0 1-2.7 3.6M6.3 6.4A17 17 0 0 0 2 11.4s3.6 7 10 7a9.9 9.9 0 0 0 4-.8"/><path d="M10 10a3 3 0 0 0 4.2 4.2"/><path d="M2.5 2.5l19 19"/>',
  folder: '<path d="M21 18.5a2.5 2.5 0 0 1-2.5 2.5h-13A2.5 2.5 0 0 1 3 18.5v-12A2.5 2.5 0 0 1 5.5 4h4l2 3h6.5A2.5 2.5 0 0 1 21 9.5z"/>',
  'folder-open':
    '<path d="M3 19V6a2 2 0 0 1 2-2h4l2 3h6a2 2 0 0 1 2 2v1"/><path d="M3 19l2.6-7.2A2 2 0 0 1 7.5 10.4H21l-2.4 8.2a2 2 0 0 1-1.9 1.4H4.8A1.8 1.8 0 0 1 3 19z"/>',
  file: '<path d="M13.5 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8.5z"/><path d="M13.5 3v5.5H19"/>',
  chevron_right: '<path d="m9.5 6 6 6-6 6"/>',
  chevron_down: '<path d="m6 9.5 6 6 6-6"/>',
  'arrow-right': '<path d="M4 12h15"/><path d="m13 6 6 6-6 6"/>',
  'arrow-left': '<path d="M20 12H5"/><path d="m11 18-6-6 6-6"/>',
  refresh:
    '<path d="M20.5 5v5h-5"/><path d="M3.5 19v-5h5"/><path d="M19.1 9.5a7.5 7.5 0 0 0-12.8-3.2L3.5 9"/><path d="M4.9 14.5a7.5 7.5 0 0 0 12.8 3.2l2.8-2.7"/>',
  play: '<path d="M6 3.8 20 12 6 20.2z"/>',
  sun: '<circle cx="12" cy="12" r="4.2"/><path d="M12 2v2.2M12 19.8V22M2 12h2.2M19.8 12H22M5 5l1.6 1.6M17.4 17.4 19 19M19 5l-1.6 1.6M6.6 17.4 5 19"/>',
  moon: '<path d="M20.5 13.4A8.5 8.5 0 1 1 10.6 3.5a7 7 0 0 0 9.9 9.9z"/>',
  cloud: '<path d="M17.5 19H7a4.5 4.5 0 0 1-.9-8.9A6 6 0 0 1 17.3 9a5 5 0 0 1 .2 10z"/>',
  globe: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18"/><path d="M12 3c2.5 2.4 3.8 5.5 3.8 9S14.5 18.6 12 21c-2.5-2.4-3.8-5.5-3.8-9S9.5 5.4 12 3z"/>',
  user: '<circle cx="12" cy="8.5" r="4"/><path d="M4.5 21a7.5 7.5 0 0 1 15 0"/>',
  activity: '<path d="M22 12h-4l-3 8-3.5-16-3 8H2"/>',
  wifi: '<path d="M5 12.5a10 10 0 0 1 14 0"/><path d="M8.5 16a5.5 5.5 0 0 1 7 0"/><path d="M12 19.5h.01"/><path d="M1.8 9a15 15 0 0 1 20.4 0"/>',
  'wifi-off': '<path d="M2.5 9a15 15 0 0 1 4.2-2.6M9.5 4.5A15 15 0 0 1 21.7 9"/><path d="M5 12.5a10 10 0 0 1 3.5-2.2M15.5 11.4a10 10 0 0 1 3.5 1.1"/><path d="M12 19.5h.01"/><path d="M2.5 2.5l19 19"/>',
  package: '<path d="M21 8.2v7.6a2 2 0 0 1-1 1.7l-7 3.9a2 2 0 0 1-2 0l-7-3.9a2 2 0 0 1-1-1.7V8.2a2 2 0 0 1 1-1.7l7-3.9a2 2 0 0 1 2 0l7 3.9a2 2 0 0 1 1 1.7z"/><path d="m3.3 7.3 8.7 4.9 8.7-4.9"/><path d="M12 21v-8.8"/>',
  link: '<path d="M10.5 13.5a4 4 0 0 0 5.7 0l2.6-2.6a4 4 0 1 0-5.7-5.7l-1.2 1.2"/><path d="M13.5 10.5a4 4 0 0 0-5.7 0l-2.6 2.6a4 4 0 1 0 5.7 5.7l1.2-1.2"/>',
  external: '<path d="M14 4h6v6"/><path d="M20 4 11 13"/><path d="M18 14.5V19a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4.5"/>',
  hard_drive:
    '<path d="M21.5 12H2.5"/><path d="M5.4 4.7 2.5 11v6a2 2 0 0 0 2 2h15a2 2 0 0 0 2-2v-6l-2.9-6.3A2 2 0 0 0 16.8 3.5H7.2a2 2 0 0 0-1.8 1.2z"/><path d="M6.5 16h.01M10.5 16h.01"/>',
  calendar: '<rect x="3.5" y="5" width="17" height="16" rx="2.5"/><path d="M16 3v4M8 3v4M3.5 10.5h17"/>',
  shield_alert: '<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><path d="M12 8.5v3.5M12 15h.01"/>',
  circle: '<circle cx="12" cy="12" r="9"/>',
};
