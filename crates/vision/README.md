# 按需图片渲染原型

`vision` 将调用方提供的旧版世界状态与本地客户端 JAR 中的资源转为 PNG。
参考新版官方实现中资源解析、模型几何和场景绘制的职责划分；渲染代码为本仓
独立 Rust 实现，不包含官方 Java 代码或资源。当前实测资源目标为 26.1.2。

当前使用 CPU 三角形光栅化：视锥裁剪、透视正确 UV 插值、深度缓冲、透明合成，
按图像行并行；没有窗口、GPU 上下文、游戏模拟或持续渲染循环。
`render` 与 `Frame::png()` 提供图片字节；组合根配置本地资源后，通过
`perception::PictureTools` 注册为 `view` 工具，图片进入 agent 原生工具回执。

## 已有路径

```text
Scene（版本、相机眼睛位置、方块名与属性）
  + Resources（只读本地 client.jar）
  → blockstates variants / multipart
  → 父模型、纹理引用、elements、UV 与旋转
  → 带贴图的三角形 / 遮挡 / 透明合成
  → Frame（RGBA 图片 + Report）→ PNG
```

支持模型父级与纹理引用继承、字符串和 sprite 对象材质、variants 属性选择、
multipart 的 AND/OR 条件、元素旋转、方块朝向旋转、显式/默认 UV、面 UV 旋转、
相邻完整不透明方块的面剔除，以及透明像素和半透明表面。资源版本必须与 Scene
版本完全匹配。JAR 只读，不运行、不解包到游戏安装目录。

## 运行

合成场景验证（场景坐标是测试构造的，图片使用用户本地的官方资源）：

```sh
cargo run --release -p vision --example render_scene -- <client-26.1.2.jar> <output.png>
```

传入自己的 Scene JSON（字段参见 `Scene`、`Camera`、`Block`）：

```sh
cargo run --release -p vision --example render_scene -- <client.jar> <output.png> <scene.json>
```

通过现有协议客户端连接服务端，复制当前方块状态后渲染：

```sh
cargo run --release -p vision --features live --example capture -- <client-26.1.2.jar> <host> <port> <offline-name> <output.png | ->
```

输出路径给 `-` 时常驻在线，从标准输入逐行读输出路径，每行按当前位姿出一张图，
便于旁观同一视点做原版对照。

探针不依赖模型服务，不发游戏命令。它等连接就绪，采集视距内（客户端默认 12 区块与
服务端视距取小）全部区块的表面方块；身边 16 格内尚未加载的格子会重试，超时后报错，
不作为空气渲染。默认输出 640×360，使用实际 yaw/pitch、站姿眼高 1.62、垂直 FOV 70°。
视距尽头按原版加雾（`fog.glsl` 柱面距离、起点为视距减 clamp(视距/10, 4, 64)），
背景按原版顺序画天空盘、日出日落扇面、太阳、月亮、星星与眼低于地平线时的黑盘。
光照与昼夜按 26.1.2 客户端转录：`daylight.rs` 从客户端 jar 读主世界维度属性与
`timeline/day`、`timeline/moon` 时间轴，按场景的时钟 tick 求天空色、雾色、天空光系数、
天体角度、星光亮度与月相；`light.rs` 用场景附带的光照格做原版平滑光照与环境光遮蔽
（`BlockModelLighter`）、平面光照、流体光照与实体单点光照，再逐顶点乘光照贴图
（`lightmap.fsh`，亮度选项取默认 0.5）；`sky.rs` 画天空与雾色（含看向太阳时的日出日落偏色）。
生物群系按 26.1.2 客户端转录：`biome.rs` 按原版 `BiomeManager` 模糊取样逐方块定生物群系，
`BlockColors` 的染色源（草、树叶、枯叶、水、红石线、茎等）按 `tintindex` 取色，
生物群系染色按 `calculateBlockTint` 的 5×5 同高度平均；草与树叶色取客户端 jar 的
colormap 与生物群系 json（温度、降水、覆盖色、黑森林与沼泽修饰，沼泽用原版
`BIOME_INFO_NOISE`，`random.rs` 复刻 Java 随机数与单倍频单纯形噪声）。天空、雾、
天空光与环境光颜色在维度底值与时间轴之间叠相机处的生物群系属性（高斯权重插值）。
方块在世界读锁里整体克隆成同一时刻的副本后放锁，解码、模型解析和成像都在锁外。玩家姿态来自最新 tick 快照，方块
来自一次世界读锁下的复制，两者不是原子化的服务端同一 tick。

每张图片旁边生成 `*.report.json`，含方块数、三角形数和简化/不支持项。
图片、JAR、运行快照和服务端世界请保存在 git 忽略的 `target/` 或 `server-run/`。

## 协议采图探针

测试链路是：启动 26.1.2 服务端 → `capture` 以协议客户端登录 → 等区块加载 →
复制服务端下发的方块状态 → 渲染 PNG → 查看图片及报告。无需运行模型服务。

1. 准备一个已完成首次初始化和 EULA 接受的独立 26.1.2 测试服目录。当前探针只支持
   离线身份，本地测试服使用 `online-mode=false`、`enforce-secure-profile=false`、
   `server-ip=127.0.0.1`、`server-port=25586`，视距至少 3 个区块。复用已有测试目录
   时沿用其中的配置；不要覆盖其他服务器的配置或世界。
2. 在该目录下使用 Java 25 启动官方服务端，等待控制台报告启动完成。以下 PowerShell
   命令从仓库根目录执行，假定本地物料和测试目录已经存在；Java 路径按本机安装设置。

   ```powershell
   $java = 'C:/Program Files/Microsoft/jdk-25.0.3.9-hotspot/bin/java.exe'
   $serverJar = (Resolve-Path 'supplies/mojang/26.1.2/server.jar').Path
   Push-Location 'target/vision-server'
   try { & $java -Xms512M -Xmx2G -jar $serverJar nogui }
   finally { Pop-Location }
   ```

3. 在另一终端的仓库根目录执行，使用新的图片名保留此前结果：

   ```powershell
   cargo run --release -p vision --features live --example capture -- supplies/mojang/26.1.2/client.jar 127.0.0.1 25586 VisionProbe target/vision-live-raster.png
   ```

   探针最多等待连接就绪 45 秒，再等待周围区块加载 30 秒。采集完成后自动断开。
   图片来自这个协议客户端角色的位置和视角；它不会自动移动到某个测试建筑，
   也不会继承其他玩家的视角。测试位置、朝向及场景需要在测试服侧预先安排。
4. 打开 `target/vision-live-raster.png` 及同名 `.report.json`。检查朝向、墙体遮挡、
   玻璃后方可见部分、楼梯/半砖高度、栅栏间隙及占位块报告。有原版客户端时可在
   同一位置、朝向和匹配 FOV 下做画面对照；已知简化见“当前限制”。
5. 保存程序结果与图片一起作为本地证据，记录构建模式、资源版本、方块/三角形数。
   `capture` 打印的时间包含资源处理、网格构建、成像、PNG 编码及文件写入；
   **不含**编译、JAR 打开、连接、等区块、方块复制及 Scene 装配。因此不能直接
   与 `benchmark` 的纯成像计时比较。要量端到端，应先编译，再单独计时运行二进制。
6. 在测试服控制台输入 `stop`，等待正常退出。

成功生成图片证明真实协议状态走通了出图链路；仍需人工检查画面，不能只凭退出码
或文件存在证明渲染正确。这个流程不会自动安排场景、执行截图断言或与原版逐像素比对。

## bot 与假玩家的完整交互测试

这里运行实际 `companion` 与真实视觉模型：假玩家向 bot 发消息，bot 自行调用
`view` 收到图片，再通过 `chat_box` 给出观察和工具反馈。单独运行 `capture` 不覆盖
这条链路。当前可用 DeepSeek `deepseek-flash` + Responses；官方说明见
[图片工具回执](https://api-docs.deepseek.com/guides/responses_api/)。

服务端沿用上面的独立 26.1.2 本地测试服。在仓库根目录先构建：

```powershell
cargo build -p companion -p world --features world/azalea --bin companion --example fake_player
```

在 bot 终端配置并启动（密钥文件路径按本机设置；记忆使用独立测试文件）：

```powershell
$env:MINEINTENT_HOST = '127.0.0.1'
$env:MINEINTENT_PORT = '25586'
$env:MINEINTENT_USERNAME = 'VisionBot'
$env:MODEL_PROTOCOL = 'responses'
$env:MODEL_ENDPOINT = 'https://api.deepseek.com/responses'
$env:MODEL_NAME = 'deepseek-flash'
$env:MINEINTENT_MODEL_API_KEY_FILE = 'C:/path/to/deepseek-key.txt'
$env:MINEINTENT_CLIENT_JAR = (Resolve-Path 'supplies/mojang/26.1.2/client.jar').Path
$env:MINEINTENT_MEMORY_FILE = 'target/vision-bot-memory.local.md'
$env:MINEINTENT_TRACE_FILE = 'target/vision-bot-trace.local.txt'
& target/debug/companion.exe
```

确认 bot 进入世界，工具表包含 `view`。在独立测试服里安排可见的
方块场景和 bot 朝向后，在另一终端让假玩家进服发消息并倾听：

```powershell
& target/debug/examples/fake_player.exe 127.0.0.1 25586 VisionTester 'VisionBot，请调用 view 看一下眼前的图片，再通过聊天告诉我图片里看到了什么，以及这个看图工具有哪些不清楚或不好用的地方。' 120
```

必要时继续用假玩家追问：让 bot 转头后再次看图，确认反馈随新视角变化；询问方块
形状、相对布局和遮挡，避免只问已从文字状态获得的坐标或名字。处境文字（位置、手持等）
仍在投递，所以仅凭回答对了不能证明模型使用了图片。

检查诊断轨迹中的 `view {}` 调用、成功回执中的 `[图片]` 标记，以及假玩家收到的
bot 聊天反馈。`[图片]` 只证明工具产出了图片；还需要紧接着的模型请求正常结束和
针对图像的回复，才能取得完整交互证据。本地 HTTP 自动测试另外核对工具回执实际
编码为 `function_call_output` 内的 `input_image` 并保持 call ID。

当前 `view` 固定 640×360、按视距出图、站姿眼高；光照取服务端光照，昼夜取当前时刻。
资源缓存复用，图像不落盘、不上传成文件，只随模型请求发送。历史图片会占用会话
体积，尚无专门的图片淘汰策略。测试结束先在 bot 终端 Ctrl+C，再在服务端输入 `stop`。
聊天和诊断轨迹只保存在本地忽略目录，不进入仓库。

## 成像与测量

`raster.rs` 将模型三角形变换到相机空间，裁剪六个视锥平面后投影；只扫描三角形
覆盖的像素矩形。共享边使用 top-left 覆盖规则，UV 使用透视正确插值，完全透明的
纹素不写深度。最靠近的不透明像素写深度缓冲，半透明片元逐像素按深度保留最近
16 层，最终从前向后合成。按 16 行处理临时缓冲，最多 8 个线程；远距离仍按眼睛
到像素对应表面的距离限制。近裁剪平面位于相机前方 0.0001 格。

重复出图应复用 `Resources`，避免每次重新打开 JAR，保留已加载的模型和纹理。
场景几何仍逐次生成。可重复的两组基准（321 方块与 2889 方块合成场景）使用：

```sh
cargo run --release -p vision --example benchmark -- <client-26.1.2.jar>
```

每组输出单独的 JAR 打开耗时、6 次成像耗时（第一次资源冷缓存，其后复用缓存）、
6 次 PNG 编码耗时，单位毫秒。成像包含资源处理与网格构建；不含 JAR 打开、PNG、
文件写入、联网或编译。这是合成场景测量，不是实服端到端耗时或实时帧率。

需要与旧射线算法在同一次运行里比较时，设置 `MINEINTENT_CLIENT_JAR` 为本地
26.1.2 JAR 的绝对路径（测试工作目录是 crate 目录），再运行下面的手动基准。两条路径预热资源后交替执行，各量
10 次，输出原始毫秒数；不设不稳定的性能通过阈值。

```sh
cargo test --release -p vision benchmark_ray_and_raster_with_local_resources -- --ignored --nocapture
```

## 当前限制

- 实体（`entity.rs`）：按 26.1.2 客户端模型类转录几何（猪、牛、羊及羊毛、鸡、苦力怕、蜘蛛、
  僵尸、骷髅、玩家默认皮肤），掉落物画成 1/4 方块或正对镜头的物品贴图。静止姿态（僵尸举臂
  除外）、成年模型、默认花色，无装备与手持物；其余实体画成碰撞箱大小的紫黑占位块并列入报告。
- 尚未绘制方块实体内容、手持物、天气和粒子。HUD 只画准星
  （原版 `hud/crosshair` 精灵、对底色取反，`Options::crosshair` 控制），其余 HUD 走文字。
- 箱子等代码定义的特殊模型及资源读取失败会显示紫黑占位块，并在报告里列出原因。
- 只有主世界的环境属性；生物群系属性只接了天空、雾、天空光与环境光颜色（水下雾色等未接）；
  没有云、天气、方块光闪烁、虚空暗化与状态效果（夜视、黑暗）。
- 加权模型只选第一个变体；动画贴图冻结在第一个方形 tile；uvlock 暂按随模型旋转近似。
- 流体是带贴图的平面高度盒体，没有邻域水面平滑、流水 UV 或含水方块的流体几何。
- 最多合成 16 层透明表面；不是原版 OIT。尚无跨帧网格缓存。
- 只支持普通原版资源路径，尚无资源包叠加、服务端资源包和图集重定向。

这些限制意味着图片目前不能作为完整视觉感知替换现有观察能力。

## 检查

```sh
cargo test -p vision --all-targets
cargo clippy -p vision -p world --features vision/live --all-targets -- -D warnings
```

自动测试使用自制 JSON/PNG/JAR 资源，不依赖官方资源，覆盖相机坐标、版本拒绝、
模型继承、透明遮挡、variants 旋转、multipart 条件与未知模型显式占位。
测试专用 `trace.rs` 保留独立的射线求交参照，检查相机角度、视锥裁剪、透视 UV、
透明孔洞、远距离限制、透明排序与共享边不重复合成；不编入生产库。
射线的边界包含规则与光栅化 top-left 规则不同，轮廓边上的像素不保证逐字节相同；
重合的透明模型面也可能因等深度选择而不同。
真实服务端输出需另行运行 `capture`，单测不提供实服成像证据。
