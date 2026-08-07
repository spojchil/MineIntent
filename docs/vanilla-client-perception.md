# 原版客户端如何呈现「瞬时发生的事」

> 无产品权威。本文是调研记录，供设计声音通道与聊天历史工具时回看。
>
> **全部事实来自反汇编** `supplies/mojang/26.1.2/client.jar`。该 jar **未混淆**，
> 类名与常量名完整。工具：`supplies/tools/jdk-25/Contents/Home/bin/javap -p -c -constants`。
> 语言文件取自 jar 内 `assets/minecraft/lang/en_us.json`（502 KB，7886 键）。
>
> 复核命令见 §5。

## 0. 一句话

**原版客户端没有事件队列。** 玩家感知到的每一样东西，要么是**被维护的状态**，
要么是**状态上的一个倒计时**，要么是**按内容归并、按时限过期的有界列表**。

## 1. 声音 → 字幕

`net/minecraft/client/gui/components/SubtitleOverlay`

### 1.1 来源：Mojang 已经筛过一遍

```java
public void onPlaySound(SoundInstance s, WeighedSoundEvents e, float range) {
    if (e.getSubtitle() == null) return;      // ← 没配字幕的声音，什么都不做
    ...
}
```

字幕来自 `sounds.json` 每个 sound event 的 `subtitle` 字段，**可选**。26.1.2 的
`en_us.json` 里共 **998 条** `subtitles.*`：

| 前缀 | 条数 |
|---|---:|
| `subtitles.entity.*` | 722 |
| `subtitles.block.*` | 166 |
| `subtitles.item.*` | 92 |
| ambient / weather / ui / event / particle / enchant / chiseled_bookshelf | 18 |

文本一律是**主语 + 动词**的自然语言，不是标识符：

```
Allay seeks        Armadillo grunts    Anvil landed       Beacon hums
Rain falls         Eerie noise         Ominous horn blares
Player hurts       Player drowning     Critical attack    Block broken
```

### 1.2 归并：一种声音一行，不管响了多少次

```java
for (Subtitle s : subtitles)
    if (s.getText().equals(newText)) { s.refresh(newPos); return; }   // 不新增
subtitles.add(new Subtitle(text, range, pos));
```

**按字幕文本去重。** `Subtitle` 内部只维护「这条字幕在哪些位置响过」：

```java
void refresh(Vec3 pos) {
    playedAt.removeIf(p -> p.location().equals(pos));   // 同位置：换新时间
    playedAt.add(new SoundPlayedAt(pos, Util.getMillis()));
}
```

一个走动的生物踩 40 步 → `block.generic.footsteps`（文本 `"Footsteps"`）**一条**，
内部记 40 个位置，屏幕上一行。

注意 `subtitles.block.generic.footsteps` 是**所有方块类型共用**的一条文本——
在 `sounds.json` 层就已经归并过一次。

### 1.3 时限：3 秒，且逐帧重判可听

```java
private static final long DISPLAY_TIME = 3000L;
```

每帧 `extractRenderState`：

1. `isAudibleFrom(listenerPos)` 过滤——`range` 为无穷则恒真；否则取**最近**一次
   播放位置比距离
2. `purgeOldInstances(3000.0)`——删掉 `now - time > 3000` 的位置记录
3. `isStillActive()`（还剩位置记录吗）为假 → 整条移除

**走远了立刻消失**，不等 3 秒。透明度随时间从 `255.0f` 衰减到 `75.0f`。

反汇编里**没有条数上限**。同时显示的行数 = 最近 3 秒内响过、且此刻仍可听的
**不同字幕文本数**——上界是 998，实际同屏通常个位数。

### 1.4 方向：三态，极粗

```java
Vec3 dir      = closest.location().subtract(listenerPos).normalize();
double rightDot   = right.dot(dir);
double forwardDot = forward.dot(dir);
boolean inFront   = forwardDot > 0.5;        // 视线 60° 锥内
```

- `inFront` → **不显示箭头**
- 否则按 `rightDot` 符号 → `>` 或 `<`

**没有距离、没有角度、没有坐标。** 只有「前方 / 左 / 右」三态，方向取自
`getClosest` 选出的**最近一次**播放位置。

## 2. 聊天

`net/minecraft/client/gui/components/ChatComponent`
与 `net/minecraft/client/multiplayer/chat/GuiMessage`

```java
private static final int MAX_CHAT_HISTORY = 100;
private static final int TIME_BEFORE_MESSAGE_DELETION = 60;
```

一条消息带什么（`GuiMessage` 是 record）：

| 字段 | 说明 |
|---|---|
| `addedTime: int` | **客户端 tick 计数**，只用于淡出，**不显示** |
| `content: Component` | 正文 |
| `signature: MessageSignature` | 签名，用于验证与删除 |
| `source: GuiMessageSource` | **`PLAYER` / `SYSTEM_SERVER` / `SYSTEM_CLIENT`** 三态 |
| `tag: GuiMessageTag` | 可信度标记（未签名 / 已修改等） |

**原版聊天不显示时间戳。** 顺序是列表顺序，隐含。

可见行数由 `chatHeightUnfocused` / `chatHeightFocused` 两个高度选项推出（可配），
不聚焦时淡出；**打开聊天即可回滚到全部 100 条**。

也就是说：**近处一瞥（有限、会淡出） + 按需回看（全量 100 条）** 是两个通道，
而不是同一个队列的深浅。

## 3. 受伤

`net/minecraft/world/entity/LivingEntity`

```java
public int hurtTime;          // 倒计时，驱动红色闪烁
public int hurtDuration;
protected float lastHurt;
private EntityReference<LivingEntity> lastHurtByMob;
protected EntityReference<Player> lastHurtByPlayer;
```

三条通道并存，**没有一条是队列**：

1. **状态**：`health` 是当前值
2. **状态上的倒计时**：`hurtTime` 递减，驱动红闪；「我刚被打」是一个会自己消失的状态
3. **状态**：`lastHurtByMob` / `lastHurtByPlayer`——「谁打的我」是被维护的引用，不是事件
4. **字幕**：`Player hurts` / `Player burns` / `Player drowning` 走 §1 的通道

## 4. 对我们的意义

### 4.1 声音的量级问题消解了

我此前记的待验证项「声音的真实量级没实测」，被字幕机制回答了：

| 原始声音 | 字幕层做的事 | 到达上限 |
|---|---|---|
| 40 次脚步 | 按文本归并 | 1 条 |
| 没配 `subtitle` 的 | 直接丢弃 | 0 条 |
| 3 秒前的 | 过期删除 | 0 条 |
| 超出 `range` 的 | 逐帧重判 | 0 条 |

**「哪些声音该到模型」这个问题，Mojang 已经替我们答过了**——就是那 998 条。

### 4.2 它是状态，不是流

`SubtitleOverlay` 维护的是「此刻该显示什么」，每帧重算。这和我们现在
`SoundHistory` 存原始声音事件（ID、类别、坐标、音量、音高，无归并、无字幕文本）
不是一回事：模型现在收到的是**协议原料**，而不是玩家真正感知到的那一行。

### 4.3 聊天历史工具（W08b）有了现成形状

> **W08b｜已确认**：公屏历史工具应当让其保存范围内的**全部**消息可查。

原版就是这么做的：100 条上界、带 `source` 三态、不带时间戳、按需回看。我们当前
6 个工具里**没有这个工具**，聊天因此被塞进事件通道——而事件通道会丢它。

### 4.4 一条判据

原版对每一样瞬时事物都做了同一件事：**把「发生过什么」折叠成「现在该让玩家知道
什么」**。折叠的方式各不相同（字幕按文本归并、聊天按条数截断、受伤按倒计时衰减），
但方向一致——**没有任何一处保留了「待处理事件」的队列。**

## 5. 复核命令

```sh
J=supplies/mojang/26.1.2/client.jar
JAVAP=supplies/tools/jdk-25/Contents/Home/bin/javap

# 字幕机制
unzip -o -j "$J" 'net/minecraft/client/gui/components/SubtitleOverlay*.class' -d /tmp/mc
"$JAVAP" -p -c -constants /tmp/mc/SubtitleOverlay.class

# 聊天
unzip -o -j "$J" 'net/minecraft/client/gui/components/ChatComponent.class' \
                 'net/minecraft/client/multiplayer/chat/GuiMessage*.class' -d /tmp/mc
"$JAVAP" -p -constants /tmp/mc/ChatComponent.class

# 998 条字幕
unzip -o -j "$J" assets/minecraft/lang/en_us.json -d /tmp/mc
python3 -c "import json;d=json.load(open('/tmp/mc/en_us.json'));\
print(sum(1 for k in d if k.startswith('subtitles.')))"
```

## 6. 本文没有查的

1. 淡出曲线的精确公式（`280.0/40.0`、`160.0/20.0` 两组常量的用途）——与设计无关，
   没有追。
2. `sounds.json` 本身不在 jar 里（是资产对象），所以「有多少 sound event **没有**
   字幕」这个数没拿到。只知道有字幕的是 998 条。
3. 告示牌文字的到达路径——维护者已裁定它是特殊方块，走方块准入，故未查。
4. 别人的私聊：客户端层面不存在这个概念，服务端决定发给谁；未追。
