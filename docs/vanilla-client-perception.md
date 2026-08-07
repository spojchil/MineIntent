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

字幕来自 `sounds.json` 每个 sound event 的 `subtitle` 字段，**可选**。

**从 1902 种声音收敛到 926 行，是三级归并：**

| 层级 | 数量 | 归并方式 | 出处 |
|---|---:|---|---|
| sound event | **1902** | —— | azalea `azalea-registry/src/builtin.rs` 的 `SoundEvent` 枚举（见 §4） |
| subtitle 键 | **998** | 多个事件可共用一个键，也可以没有键 | `en_us.json` 的 `subtitles.*` |
| **不同字幕文本** | **926** | 多个键可共用一段文本 | 上表去重 |
| 同屏行数 | **≤926** | `SubtitleOverlay` 按文本 `equals` 再去重 | §1.2 |

> **计数订正。** 从 jar 的 `SoundEvents.class` 常量池只能提出 **1807** 条，那是个
> 下界：cat / wolf / pig / cow / chicken 的声音变体族与 goat_horn 在 26.1.2 里改成
> 了数据驱动的 `Map<XxxSoundVariants$SoundSet, …>` 字段，ID 不作为常量池字符串出现，
> 共 95 条被漏掉。jar 提出的 1807 条是 azalea 1902 条的**真子集**（交集 1807，
> jar 独有 0）。

按名字直接匹配，事件里约 928 个有同名字幕键。剩下的是**下界不是真值**——共用键的
情况按名字匹配不上，已知例子：所有方块的脚步声共用 `block.generic.footsteps`，
各下界生物群系的环境音共用 `ambient.cave`。真正的映射在 `sounds.json` 里（见 §4.3）。

**这个不确定性不影响结论**：无论多少事件有字幕，能出现的不同行**至多 926 种**，
这个数是精确的。

键的分布：

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

**按字幕文本去重**——注意是文本，不是声音种类。所以铁活板门与普通活板门都出
`"Trapdoor closes"`，屏幕上**合成一行**；7 种鞍具都出 `"Saddle equips"`。

`Subtitle` 内部只维护「这条字幕在哪些位置响过」：

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
**不同字幕文本数**——绝对上界 926，实际同屏通常个位数。

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

## 4. azalea 已经给了我们什么

工作树：`~/.cargo/git/checkouts/azalea-*/d0cc847`（我们钉住的 fork）。

### 4.1 已有：声音事件

`azalea-registry/src/builtin.rs` 的 `SoundEvent` 枚举，**1902 条**，带类型化变体与
注册名：

```rust
enum SoundEvent {
    EntityAllayAmbientWithItem => "entity.allay.ambient_with_item",
    ...
}
```

包体是 `ClientboundSound`（`azalea-protocol/src/packets/game/c_sound.rs`）：

```rust
pub struct ClientboundSound {
    pub sound: azalea_registry::Holder<SoundEvent, CustomSound>,
    pub source: SoundSource,     // Master/Music/Records/Weather/Blocks/
                                 // Hostile/Neutral/Players/Ambient/Voice
    pub x: i32, pub y: i32, pub z: i32,   // 定点，不是 BlockPos
    pub volume: f32, pub pitch: f32, pub seed: u64,
}
```

### 4.2 已有：字幕文本本身

`azalea-language/src/en_us.json` 与 jar 内 `assets/minecraft/lang/en_us.json`
**内容完全一致**：

| | 总键数 | `subtitles.*` | 键差异 | 文本差异 |
|---|---:|---:|---:|---:|
| jar | 7886 | 998 | — | — |
| azalea | 7886 | 998 | **0** | **0** |

也就是说 **926 段字幕文本我们已经在依赖里了**，不需要额外下载。
`azalea-chat/src/translatable_component.rs` 还带翻译键展开的机制。

> ⚠ 易混：`azalea-protocol/.../c_set_subtitle_text.rs` 是**标题下方的副标题**
> （Title/Subtitle 屏幕文字）那个包，与声音字幕无关。

### 4.3 唯一缺的：event → subtitle 键的映射

这个映射在 `sounds.json` 里，而它：

- **不在 client.jar 里**——`assets/minecraft/` 第一层确认没有，它是单独下载的资产对象
- **不在 azalea 里**——azalea 是无头客户端，不加载资产

按名字约定能对上约一半（`entity.allay.death` → `subtitles.entity.allay.death`），
另一半靠共用键，对不上。要精确映射，需要按资产索引取一次 `sounds.json`——一次性
构建期产物，可以像 `client.jar` 一样入 `supplies/`。

**当前差距因此非常具体**：我们已经有事件、有坐标、有类别、有全部字幕文本；差的是
一张 1902 → 998 的映射表。

## 5. 对我们的意义

### 5.1 声音的量级问题消解了

我此前记的待验证项「声音的真实量级没实测」，被字幕机制回答了：

| 原始声音 | 字幕层做的事 | 到达上限 |
|---|---|---|
| 40 次脚步 | 按文本归并 | 1 条 |
| 没配 `subtitle` 的 | 直接丢弃 | 0 条 |
| 不同事件同一文本 | 按文本合并 | 1 条 |
| 3 秒前的 | 过期删除 | 0 条 |
| 超出 `range` 的 | 逐帧重判 | 0 条 |

**「哪些声音该到模型」这个问题，Mojang 已经替我们答过了**——就是那 926 段文本，
而且是从 1902 种声音三级归并下来的。

### 5.2 它是状态，不是流

`SubtitleOverlay` 维护的是「此刻该显示什么」，每帧重算。这和我们现在
`SoundHistory` 存原始声音事件（ID、类别、坐标、音量、音高，无归并、无字幕文本）
不是一回事：模型现在收到的是**协议原料**，而不是玩家真正感知到的那一行。

### 5.3 聊天历史工具（W08b）有了现成形状

> **W08b｜已确认**：公屏历史工具应当让其保存范围内的**全部**消息可查。

原版就是这么做的：100 条上界、带 `source` 三态、不带时间戳、按需回看。我们当前
6 个工具里**没有这个工具**，聊天因此被塞进事件通道——而事件通道会丢它。

### 5.4 一条判据

原版对每一样瞬时事物都做了同一件事：**把「发生过什么」折叠成「现在该让玩家知道
什么」**。折叠的方式各不相同（字幕按文本归并、聊天按条数截断、受伤按倒计时衰减），
但方向一致——**没有任何一处保留了「待处理事件」的队列。**

## 6. 复核命令

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

# 998 条字幕键 / 926 段不同文本（jar 与 azalea 结果一致）
unzip -o -j "$J" assets/minecraft/lang/en_us.json -d /tmp/mc
python3 -c "import json;d=json.load(open('/tmp/mc/en_us.json'));\
s={k:v for k,v in d.items() if k.startswith('subtitles.')};\
print(len(s),'键',len(set(s.values())),'段不同文本')"

# azalea 侧：1902 个 sound event，且语言文件与 jar 一致
A=~/.cargo/git/checkouts/azalea-*/d0cc847
grep -c '=> "' $A/azalea-registry/src/builtin.rs   # 全部 registry，非仅 SoundEvent
diff <(python3 -c "import json;print(json.dumps(json.load(open('/tmp/mc/en_us.json')),sort_keys=True))") \
     <(python3 -c "import json;print(json.dumps(json.load(open('$A/azalea-language/src/en_us.json')),sort_keys=True))") \
  && echo "语言文件一致"
```

## 7. 本文没有查的

1. 淡出曲线的精确公式（`280.0/40.0`、`160.0/20.0` 两组常量的用途）——与设计无关，
   没有追。
2. `sounds.json`（event → subtitle 键的精确映射）——见 §4.3，jar 与 azalea 都没有，
   需要按资产索引单取。**但这不影响任何结论**：能出现的不同行至多 926 种，
   这个数是精确的。
3. 告示牌文字的到达路径——维护者已裁定它是特殊方块，走方块准入，故未查。
4. 别人的私聊：客户端层面不存在这个概念，服务端决定发给谁；未追。
