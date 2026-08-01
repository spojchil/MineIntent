# 物料清单（supplies/）

> 本目录不入 git（.gitignore）。本清单与校验和入 git，用于核对物料完整性。
> 准备日期：2026-07-31。准备方式：维护者一侧的 Claude 会话在有网状态下预置。
> 离线工作期缺料 → 在 进度日志.md「补给申请」登记，勿自行联网。

## 钉版（主目标）

- **协议号 775 / MC 26.1.x 线；azalea 0.16.0+mc26.1（crates.io 发布版）**
- azalea 源码参考提交：`a253702e`（"0.16.0" 发布提交，见 azalea/ 克隆）
- 26.2（协议号 776）整套物料保留作备选，见下。

## 物料

| 路径 | 内容 | 来源 | 校验 |
|---|---|---|---|
| mojang/version_manifest_v2.json | 官方版本索引 | piston-meta.mojang.com | 见 SHA256SUMS |
| mojang/26.1.2/26.1.2.json | 26.1.2 版本元数据（含 DataVersion 线索） | piston-meta | 同上 |
| mojang/26.1.2/client.jar | 官方客户端（去混淆，oracle） | piston-data | 同上 |
| mojang/26.1.2/server.jar | 官方服务端（去混淆，oracle + 对照服） | piston-data | 同上 |
| mojang/26.2.json + mojang/client.jar + mojang/server.jar | 26.2 备选整套 | piston-meta/data | 同上 |
| paper/26.1.2/paper-26.1.2-74.jar | Paper 26.1.2 build 74（STABLE） | fill.papermc.io | 官方 sha256 见 builds-26.1.2.json |
| paper/paper-26.2-87.jar | Paper 26.2 build 87（STABLE，备选） | fill.papermc.io | 官方 sha256 见 builds.json |
| paper/cache-26.1.2/ | Paper 首启缓存（离线首启必需） | 首启一次后打包 | — |
| tools/vineflower-1.12.0.jar | 反编译器 | github.com/Vineflower | 见 SHA256SUMS |
| azalea/ | azalea 全历史克隆（主线目标 26.2；0.16.0 发布提交 a253702e 目标 26.1） | github.com/azalea-rs/azalea | git 自校验 |
| mineintent-main/ | MineIntent 主仓库只读克隆（Backend 契约、场景、产品.md） | 本地克隆 | git 自校验 |
| mineintent-go-client/ | 维护者早前实现的 Go 无头客户端只读参考（协议时钟、自动应答、26.1.2/775） | 本地克隆；HEAD `2082785` | git 自校验 |
| decompiled/26.1.2-server/ | 官方 server.jar 反编译源码树（语义 oracle） | Vineflower 本地生成 | — |
| eula/EULA.html | Minecraft EULA 存档 | account.mojang.com | 见 SHA256SUMS |
| ../vendor/ + ../.cargo/config.toml | 全部 Rust 依赖 vendor（离线构建） | crates.io | cargo 内建校验 |

## 缺料（已登记补给申请）

- minecraft.wiki 协议页快照：Cloudflare 拦截脚本抓取，待维护者浏览器另存。非阻塞。

## 补充物料（准备过程中新增）

| 路径 | 内容 | 备注 |
|---|---|---|
| tools/jdk-25/ | 便携版 Temurin JDK 25.0.4（26.1+ 服务端硬要求 Java 25） | zip 原件 tools/temurin-jdk25.zip；系统另有 C:\Program Files\Microsoft\jdk-25.0.3.9-hotspot 可作备用 |
| paper/26.1.2/first-run/ | Paper 完整首启产物：cache/、libraries/、versions/、config、已生成世界 | 离线重启无需任何下载；已实测完整启动 |
| decompiled/26.1.2-server/ | 官方 server 内层 jar 的 Vineflower 反编译树，4789 个 .java，全真名 | 抽查 ServerboundMovePlayerPacket 字段名均可读 |
| mojang/26.1.2/server-inner.jar | 从官方 bundler 抽出的服务端内层 jar（16461 条目） | 反编译输入 |
| mineintent-go-client/ | 只读协议行为参考；最新提交实现每连接协议时钟、串行出站调度和 KeepAlive/Teleport/Configuration 自动应答 | 不作为本项目代码基础；用于 M1 时序和错误分类交叉核对 |
| simple-agent/ | 维护者亲写的 Rust agent 微型实现（Rig 形状：typed tool + 可步进 run + 数组顺序资源裁决） | **授权复用源**：阶段 5 AgentRunner 直接复用改造，细节见其 _供给说明.md（2026-08-02 放入，cargo test 5/5） |

## 工具链（本机已装，非 supplies 内容）

- Rust stable 1.95 + **nightly 1.99**（azalea 的 rust-toolchain.toml 要求 nightly；vendor 构建已在 nightly 下离线验证通过，4m11s）
- JDK 21（系统）+ JDK 25（supplies 便携版为主、系统 25.0.3 备用）、git、cargo
