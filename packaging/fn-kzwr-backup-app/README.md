# 酷族备份（fn-kzwr-backup）· 飞牛应用包源码

本目录为**可提交的飞牛应用包源码**，是 `.fpk` 打包的源。

应用身份（`manifest`）：

| 字段 | 值 |
|------|-----|
| appname | `fn-kzwr-backup` |
| display_name | 酷族备份 |
| maintainer / distributor | MianJu |
| maintainer_url | https://github.com/MianJu28/fn-kzwr-backup |

> 注意：`appname` 决定安装目录与升级身份；**改名后飞牛会视为新应用**（不会原地升级旧的 `fn-kzwr-backup`）。
> 包内二进制仍名为 `fn-kzwr-backup`（与 `backend/Cargo.toml` 的 `[[bin]]` 一致），改二进制名需同步 CI 与构建脚本。

```
packaging/fn-kzwr-backup-app/
├── manifest                    # 应用元数据（见 docs/fnnas-dev-docs/core-concepts/02-manifest.md）
├── ICON.PNG / ICON_256.PNG     # 应用中心图标（128 / 256，圆角透明）
├── app/                        # → $TRIM_APPDEST
│   ├── ui/config               # 桌面入口：统一网关（gatewayPrefix=/app/fn-kzwr-backup + gatewaySocket=app.sock）
│   ├── ui/images/              # 入口图标
│   ├── bin/fn-kzwr-backup         # Rust 后端二进制
│   └── www/                    # 前端构建产物（Svelte dist）
├── cmd/                        # 生命周期脚本（见 core-concepts/01-framework.md）
├── config/
│   ├── privilege               # run-as=package, user/group=fnosbackup
│   └── resource                # data-share: fn-kzwr-backup/restore
└── wizard/                     # install/config/upgrade/uninstall（JSON 步骤数组）
```

**访问模型（统一网关，ADR-012）**：桌面入口经飞牛统一网关访问——`app/ui/config` 声明 `gatewayPrefix`/`gatewaySocket`，`cmd/main` 把 `GATEWAY_PREFIX=/app/fn-kzwr-backup` 与 `TRIM_APP_SOCK=$TRIM_APPDEST/app.sock` 注入后端进程（并在启动前/停止后清理 socket 文件）。该前缀在四处必须保持一致：`ui/config`、`cmd/main`、后端 `gateway_prefix()`、前端 `lib/appBase.js` + `vite.config.js` 的 `base`。改前缀后需**升级安装**才生效。

构建：`Scripts/build_fnos_app.sh`（详细步骤见仓库根 `README.md` 与 `docs/ARCHITECTURE.md` 第 11.6 节）。
