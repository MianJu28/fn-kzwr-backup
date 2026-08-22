# 飞牛应用开放平台开发文档

抓取自 [https://developer.fnnas.com/docs/guide/](https://developer.fnnas.com/docs/guide/)（飞牛 fnOS 应用开放平台），供本地开发时快速查阅。

- 来源站点：Docusaurus 构建，本文档内容抓取自其 `llms-full.txt`（AI 友好快照）
- 抓取日期：2026-08-22
- 版权：广州铁刃智造技术有限公司（粤ICP备2023020469号）

## 目录结构

| 子目录 | 说明 |
|--------|------|
| `quick-start/` | 快速开始（欢迎、准备工作、创建/测试/上架应用、更新日志） |
| `core-concepts/` | 开发指南（应用框架、Manifest、权限、资源、入口、网关、向导等核心概念） |
| `examples/` | 应用案例（Native 应用、Docker 应用） |
| `cli/` | 开发工具（appcenter-cli、fnpack） |
| `api/` | 开放 API（概述、调用方式、授权与文件、页面能力、错误码） |

## 快速开始 (quick-start)

| 文件 | 内容 |
|------|------|
| [00-welcome.md](quick-start/00-welcome.md) | 欢迎加入飞牛应用开发者平台（平台介绍、开发动机、学习路径） |
| [01-prerequisites.md](quick-start/01-prerequisites.md) | 准备工作：测试设备、开发电脑、访问权限、CLI 工具 |
| [02-create-application.md](quick-start/02-create-application.md) | 创建应用：从零创建一个最小 fnOS 应用包 |
| [03-test-application.md](quick-start/03-test-application.md) | 测试应用：安装并验证应用包 |
| [04-publish-application.md](quick-start/04-publish-application.md) | 上架应用：准备发布材料并发布 |
| [zz-update-log.md](quick-start/zz-update-log.md) | 更新日志 |

## 开发指南（核心概念）(core-concepts)

| 文件 | 内容 |
|------|------|
| [01-framework.md](core-concepts/01-framework.md) | 应用框架：目录结构、生命周期脚本、安装/升级/卸载/配置流程 |
| [02-manifest.md](core-concepts/02-manifest.md) | Manifest：应用元数据、兼容范围、运行方式 |
| [03-environment-variables.md](core-concepts/03-environment-variables.md) | 环境变量：运行时提供的各类环境变量 |
| [04-privilege.md](core-concepts/04-privilege.md) | 应用权限：运行用户、用户组、Root 模式、文件访问 |
| [05-resource.md](core-concepts/05-resource.md) | 应用资源：共享数据目录、系统链接、Docker 项目 |
| [06-app-entry.md](core-concepts/06-app-entry.md) | 应用入口：桌面图标、文件打开方式、访问模型 |
| [07-index-cgi.md](core-concepts/07-index-cgi.md) | index.cgi：通过 CGI 提供轻量 UI |
| [08-gateway-registration.md](core-concepts/08-gateway-registration.md) | 统一网关：注册路由、网关鉴权、WebSocket |
| [09-wizard.md](core-concepts/09-wizard.md) | 用户向导：安装/升级/卸载/配置表单定义 |
| [10-dependency.md](core-concepts/10-dependency.md) | 应用依赖：声明依赖及版本要求 |
| [11-middleware.md](core-concepts/11-middleware.md) | 中间件服务：Redis、MinIO、RabbitMQ |
| [12-runtime.md](core-concepts/12-runtime.md) | 运行时环境：Python、Node.js、Java 打包运行时 |
| [13-icon.md](core-concepts/13-icon.md) | 图标：包图标与应用入口图标规范 |

## 应用案例 (examples)

| 文件 | 内容 |
|------|------|
| [01-native.md](examples/01-native.md) | Native 应用案例：从零创建可运行的 Native 应用包 |
| [02-docker.md](examples/02-docker.md) | Docker 应用案例：从零创建可运行的 Docker 应用包 |

## 开发工具（CLI）(cli)

| 文件 | 内容 |
|------|------|
| [01-appcentercli.md](cli/01-appcentercli.md) | appcenter-cli：命令行管理应用安装和测试 |
| [02-fnpack.md](cli/02-fnpack.md) | fnpack：创建和打包飞牛 fnOS 应用 |

## 开放 API (api)

| 文件 | 内容 |
|------|------|
| [01-overview.md](api/01-overview.md) | 概述：开放能力范围、接入对象、文档组织方式 |
| [02-calling.md](api/02-calling.md) | 调用方式：前端 JS SDK 和后端 API 的通用调用规则 |
| [03-platform-config.md](api/03-platform-config.md) | 平台配置：读取宿主语言、主题、系统版本等配置 |
| [04-authorization-overview.md](api/04-authorization-overview.md) | 授权与文件概览 |
| [05-authorization-shared-access.md](api/05-authorization-shared-access.md) | 应用共享授权路径 |
| [06-authorization-user-access.md](api/06-authorization-user-access.md) | 用户个人授权路径 |
| [07-authorization-file-acl.md](api/07-authorization-file-acl.md) | 文件权限检查 |
| [08-authorization-path-convert.md](api/08-authorization-path-convert.md) | 路径转换 |
| [09-page-routing.md](api/09-page-routing.md) | 页面路由：打开文件、文件详情、文件管理器、设置页、URL |
| [10-page-ui.md](api/10-page-ui.md) | 页面交互：设置标题、监听主题/语言、关闭页面、离开提示 |
| [11-error-codes.md](api/11-error-codes.md) | 错误码：常见错误码和处理建议 |

## 使用说明

- 各文档为独立 Markdown 文件，可直接被后续开发任务引用。
- 若需获取最新文档，可重新抓取 `https://developer.fnnas.com/llms-full.txt` 后运行 `.codebuddy/tmp/split_docs.py` 重新整理。
