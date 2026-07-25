---
status: reference
authority: informative
implementation: current
last_verified: 2026-07-23
---

# `mc.ps1` 使用说明

`mc.ps1` 是当前 Paper 1.21.1 服务端的命令行管理入口。它通过 Python 管理器在后台无窗口运行 Java，并允许安全停止服务器、查看日志和发送 Minecraft 控制台命令。

## 环境要求

- Windows PowerShell 或 PowerShell 7
- Java 21
- Python 3.10 或更高版本
- 当前目录中存在 Paper 服务端 JAR

所有命令都应在服务端目录中执行：

```powershell
cd C:\path\to\maineintent\mcserver
```

查看完整命令帮助：

```powershell
.\mc.ps1 --help
```

## 首次初始化

首次运行下面的命令会启动一次 Paper，生成 `eula.txt` 和 `server.properties`，随后因尚未接受 EULA 自动退出：

```powershell
.\mc.ps1 init
```

阅读 [Minecraft EULA](https://aka.ms/MinecraftEULA)。如果接受，将 `eula.txt` 中的：

```properties
eula=false
```

改成：

```properties
eula=true
```

EULA 只需处理一次。

## 启动服务器

```powershell
.\mc.ps1 start
```

服务器会在后台无窗口运行，终端可以直接关闭。运行输出写入：

```text
logs\console-latest.log
```

重复执行 `start` 不会启动第二个实例，而是提示服务器已经运行。

## 查看状态

```powershell
.\mc.ps1 status
```

输出示例：

```text
运行中 | PID 37404 | 00:12:36 | paper-1.21.1-133.jar
```

其中依次为 Java 进程 PID、运行时长和当前服务端 JAR。

## 查看日志

显示最近约 80 行控制台日志：

```powershell
.\mc.ps1 logs
```

持续查看新增日志：

```powershell
.\mc.ps1 logs -f
```

按 `Ctrl+C` 只会退出日志查看，服务器仍会继续运行。

## 发送 Minecraft 命令

使用 `send` 向正在运行的服务端发送一条控制台命令。命令不需要以 `/` 开头。

```powershell
.\mc.ps1 send "list"
.\mc.ps1 send "say 大家好"
.\mc.ps1 send "op 玩家名"
.\mc.ps1 send "whitelist add 玩家名"
.\mc.ps1 send "time set day"
.\mc.ps1 send "save-all flush"
```

含有空格的命令建议整体放在引号中。

## 交互命令模式

```powershell
.\mc.ps1 console
```

进入后可以连续输入 Minecraft 控制台命令：

```text
> list
> say 服务器将在五分钟后重启
> save-all
```

输入以下任意命令可脱离交互模式，但不会关闭服务器：

```text
:detach
:quit
:exit
```

也可以按 `Ctrl+C` 脱离。交互模式主要用于输入命令；如需同时实时观察服务端输出，可以另开一个 PowerShell 窗口运行：

```powershell
.\mc.ps1 logs -f
```

## 安全停止服务器

```powershell
.\mc.ps1 stop
```

管理器会向 Paper 控制台发送 `stop`，等待玩家数据和世界保存完成，然后关闭后台管理进程。不要直接在任务管理器中结束 Java，否则可能损坏未保存的数据。

默认最多等待 60 秒。大型世界可以延长等待时间：

```powershell
.\mc.ps1 stop --timeout 120
```

## 修改内存和 Java 参数

编辑 `mc-config.json`：

```json
{
  "jar": "paper-1.21.1-133.jar",
  "java": "java",
  "minMemory": "2G",
  "maxMemory": "4G",
  "javaArgs": [
    "-XX:+UseG1GC"
  ],
  "serverArgs": [
    "--nogui"
  ]
}
```

- `jar`：需要启动的 Paper JAR 文件名。
- `java`：Java 命令或 `java.exe` 的完整路径。
- `minMemory`：Java 初始堆内存，即 `-Xms`。
- `maxMemory`：Java最大堆内存，即 `-Xmx`。
- `javaArgs`：额外的 JVM 参数。
- `serverArgs`：传给 Paper 的启动参数。

例如将最大内存调整为 6 GiB：

```json
"maxMemory": "6G"
```

修改配置后需要安全停止并重新启动服务器：

```powershell
.\mc.ps1 stop
.\mc.ps1 start
```

不要把 `maxMemory` 设置为电脑的全部内存，应给 Windows 和其他程序保留足够空间。

## 服务端设置

Minecraft 服务设置保存在 `server.properties`。常见项目包括：

```properties
server-port=25565
motd=A Minecraft Server
max-players=20
online-mode=true
white-list=false
difficulty=easy
gamemode=survival
view-distance=10
simulation-distance=10
```

修改 `server.properties` 后应重启服务器。建议保持：

```properties
online-mode=true
```

这会使用 Mojang/Microsoft 正版身份验证，避免玩家身份被冒用。

## 文件与目录

```text
mc.ps1                    PowerShell 命令入口
mc_manager.py             Python 后台管理器
mc-config.json            启动和内存配置
eula.txt                  Minecraft EULA 状态
server.properties         Minecraft 服务端设置
paper-1.21.1-133.jar      Paper 服务端
logs\                     Paper 和管理器日志
plugins\                  Paper 插件目录
world\                    主世界
world_nether\             下界
world_the_end\            末地
.mc-server\               管理器运行状态和本地控制信息
```

`.mc-server` 中包含运行期间使用的本地随机控制令牌，不要在服务器运行时修改或共享其中的文件。

## 崩溃后的状态清理

如果电脑断电、Python 管理进程崩溃，或状态文件未正常删除，命令可能提示管理进程没有响应。确认 Java 服务端确实已经停止后执行：

```powershell
.\mc.ps1 cleanup
```

如果服务端仍正常运行，`cleanup` 会拒绝删除状态。

清理后可以重新启动：

```powershell
.\mc.ps1 start
```

## 常用操作流程

正常开服：

```powershell
.\mc.ps1 start
.\mc.ps1 logs -f
```

设置管理员：

```powershell
.\mc.ps1 send "op 玩家名"
```

开启白名单并添加玩家：

```powershell
.\mc.ps1 send "whitelist on"
.\mc.ps1 send "whitelist add 玩家名"
.\mc.ps1 send "whitelist list"
```

重启服务器：

```powershell
.\mc.ps1 send "say 服务器正在重启"
.\mc.ps1 stop
.\mc.ps1 start
```

关机前安全停服：

```powershell
.\mc.ps1 stop
```

## 局域网和公网连接

- 本机加入地址：`localhost`
- 同一局域网的玩家：使用服务器电脑的局域网 IPv4 地址和端口 `25565`
- 公网玩家：通常需要路由器端口转发、防火墙规则或安全的组网/隧道方案

不要随意公开家庭公网 IP。开放公网访问前，建议启用正版验证和白名单，并做好世界备份。

## 命令速查

| 命令 | 用途 |
|---|---|
| `.\mc.ps1 --help` | 查看帮助 |
| `.\mc.ps1 init` | 首次生成 EULA 和基础配置 |
| `.\mc.ps1 start` | 后台无窗口启动服务器 |
| `.\mc.ps1 status` | 查看运行状态和时长 |
| `.\mc.ps1 logs` | 查看最近日志 |
| `.\mc.ps1 logs -f` | 持续跟踪日志 |
| `.\mc.ps1 send "命令"` | 发送一条 Minecraft 控制台命令 |
| `.\mc.ps1 console` | 连续输入控制台命令 |
| `.\mc.ps1 stop` | 保存世界并安全停止 |
| `.\mc.ps1 cleanup` | 清理崩溃后残留的管理状态 |
