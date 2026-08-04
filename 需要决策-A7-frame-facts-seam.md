# NEW-14：A7 light/armor 的原子 frame-facts seam

> 更新日期：2026-08-03  
> 状态：等待维护者确认；实现可按建议 A 继续，不阻塞 block/sound 收口

## 结论先行

建议新增一个**不参与 JSON 序列化、仅供 Rust 进程内 app/Participant 使用**的
原子 frame-facts 捕获 seam。它一次返回现有 `MinecraftSnapshotV1`、原版护甲点和
F3 光照值；真实 backend 在同一 generation 下捕获三者。不要给同名 snapshot v1
增加字段，也不要让 app 分三次读取后拼接。

## 已确认的权威语义

本地 `supplies/mojang/26.1.2/client.jar` 是 26.1.2 官方客户端。字节码核对结果：

- F3 `DebugEntryLight` 在摄像实体的 `blockPosition()` 调用
  `LevelLightEngine.getRawBrightness(position, 0)`；该方法返回
  `max(skyLight, blockLight)`，值域 0-15。
- `LivingEntity.getArmorValue()` 对 `minecraft:armor` 属性的最终值调用
  `floor`。最终属性计算顺序是：先累加 `ADD_VALUE`，再按该中间值累加
  `ADD_MULTIPLIED_BASE`，最后逐个乘 `ADD_MULTIPLIED_TOTAL`。
- `中期更新-12.md` 进一步冻结 Participant wire：armor 取 0-20，0 整键省略；
  light 必填且取 0-15。因此 backend 输出 armor 时还需夹到 0-20。

Azalea 0.16 当前 `update_attributes` 和 `light_update` handler 都是空实现，world
也不保存 light 数据；backend 必须消费 raw packet 并维护 epoch/scope 绑定缓存。

## 为什么不能直接扩展 snapshot v1

`MinecraftSnapshotV1` 是严格 `deny_unknown_fields` 的既有 wire DTO，已有 fixture 与
回放兼容要求。给同名 v1 增加 armor/light 会让相同 protocol 名称静默漂移；即使
新增字段设为 optional，也会改变新编码结果和跨版本含义。

另一方面，app 若先调用 `snapshot()`，再分别调用 `armor()` 和 `light()`，连接或
世界 generation 可在三次调用之间变化，产生跨 epoch/跨 dimension 的混合开场帧。

## 选项

### A（建议）：新增非 wire 的原子 frame-facts 捕获

在 contracts 增加普通 Rust 值类型（名称可在实现时收敛），至少包含：

- `snapshot: MinecraftSnapshotV1`；
- `armor: Option<u8>`；
- `light: Option<u8>`。

在 `MinecraftBackendApi` 增加带默认实现的方法：旧 fake/backend 默认只返回现有
snapshot，armor/light 为 `None`，绝不发明 0 或 15；真实 facade override，并由
runtime 在一个 observation generation 下复制 snapshot 与对应缓存。Participant
继续对缺失 light fail closed，armor 缺失或 0 均省略。

优点：不改变任何既有 wire；兼容现有 trait fake；能证明 epoch/dimension/position
与 light/armor 同源；未来若铸 snapshot v2，也能平滑替换内部载荷。

### B：现在铸 `MinecraftSnapshotV2`

把 armor/light 加入新 snapshot wire，扩展 contracts、fixture、BackendReady、
订阅与所有 adapter。版本上最显式，但工作面远大于 Participant 当前只需两个事实
的需求，也会把 app composition 与全局 snapshot 升级绑死。

### C：直接给 `MinecraftSnapshotV1` 增字段

实现最短，但违反同名严格 wire 稳定性，不建议。

## 建议回复

```text
NEW-14: A
```

若未及时回复，我会按 A 继续，并在最终验收中明确它是进程内 seam、不是 snapshot
协议升级。
