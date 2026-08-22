# fnos-backup · 飞牛应用包源码

本目录为**可提交的飞牛应用包源码**，是 `.fpk` 打包的源。

- 构建脚本 `../../bin/build_fnos_app.sh` 会：
  1. 构建后端（`cargo build --release`）
  2. 构建前端（`npm run build`）
  3. 将二进制拷入 `app/bin/`、前端产物拷入 `app/www/`
  4. `fnpack build` 生成 `.fpk`
- CI 工作流 `.github/workflows/build-fnos-app.yml` 基于本目录做双架构构建。

## 目录结构

```
packaging/fnos-backup-app/
├── manifest                    # 应用元数据（见 docs/fnnas-dev-docs/core-concepts/02-manifest.md）
├── ICON.PNG / ICON_256.PNG     # 应用中心图标
├── app/                        # 应用运行文件 → 安装后为 $TRIM_APPDEST
│   ├── ui/
│   │   ├── config              # 桌面入口（iframe → http://localhost:8080/）
│   │   └── images/             # 入口图标
│   └── (bin / www 由构建脚本生成)
├── cmd/                        # 生命周期脚本（见 core-concepts/01-framework.md）
├── config/
│   ├── privilege               # run-as=package, user/group=fnosbackup
│   └── resource                # data-share: fnos-backup/restore
└── wizard/                     # install/config/upgrade/uninstall（JSON 步骤数组）
```

## 飞牛规范要点

| 项 | 取值 | 参考文档 |
|----|------|---------|
| 应用形态 | 普通应用（非 Docker），端口服务 UI | ADR-008 / 选型 5 |
| platform | `x86`（安装时按架构选二进制） | manifest.md |
| ctl_stop | `true`（服务类应用） | manifest.md |
| service_port | `8080` | manifest.md |
| 运行身份 | `run-as=package`，用户 `fnosbackup` | privilege.md |
| 源目录授权 | `disable_authorization_path=false` | privilege.md |
| 数据/配置 | `$TRIM_PKGVAR` / `$TRIM_PKGETC` | environment-variables.md |
| 向导 | HTTP 端口 + 管理员口令（JSON 步骤数组） | wizard.md |

## 构建

```bash
../../bin/build_fnos_app.sh
```
