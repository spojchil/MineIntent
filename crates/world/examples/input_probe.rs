//! 键鼠输入实服探针：按一串 `Module::input`，打印每次松开的回执（旁证）。
//! 服务端方块/位置/实体查证（权威源）由运行者经 rcon 在每步之间另行采集。
//!
//! 场景（编排方经 rcon 布置，超平坦世界）：探针上线后被 tp 到场地中央、朝南水平，
//! hotbar.0 钻镐、hotbar.1 圆石、hotbar.2 金苹果（饱食度满也能吃）；步骤 10 前编排方把探针放回地面、在正前方 2 格放一只无 AI 的猪。
//! 每步前打印 `[探针] 步骤 N`，编排方可按行号对齐服务端查询。
//!
//! 用法（需要 `--features azalea`）：
//! `cargo run -p world --release --features azalea --example input_probe -- <host> <port> <名字> <就位秒数>`

use std::sync::Arc;
use std::time::Duration;

use world::{ConnectionConfig, DoorCommand, HeldKeys, InputSpec, Module, MouseButton, Turn};

fn keys(names: &[&str]) -> HeldKeys {
    let mut keys = HeldKeys::default();
    for name in names {
        match *name {
            "w" => keys.forward = true,
            "s" => keys.back = true,
            "a" => keys.left = true,
            "d" => keys.right = true,
            "space" => keys.jump = true,
            "shift" => keys.sneak = true,
            "ctrl" => keys.sprint = true,
            other => panic!("未知键 {other}"),
        }
    }
    keys
}

async fn press(module: &Arc<Module>, step: u32, label: &str, spec: InputSpec) {
    println!("[探针] 步骤 {step}：{label}");
    match module.input(spec).await {
        Ok(outcome) => println!("[探针] 步骤 {step} 回执：{outcome:?}"),
        Err(reason) => println!("[探针] 步骤 {step} 拒绝：{reason}"),
    }
    // 给编排方的 rcon 查询留窗口。
    tokio::time::sleep(Duration::from_secs(3)).await;
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port: u16 = args
        .next()
        .unwrap_or_else(|| "25565".to_owned())
        .parse()
        .map_err(|error| format!("端口无效：{error}"))?;
    let username = args.next().unwrap_or_else(|| "presser".to_owned());
    let settle: u64 = args
        .next()
        .unwrap_or_else(|| "20".to_owned())
        .parse()
        .map_err(|error| format!("就位秒数无效：{error}"))?;

    let module = Arc::new(
        Module::start(ConnectionConfig {
            host,
            port,
            username,
        })
        .map_err(|error| format!("启动失败：{error}"))?,
    );
    module.wait_ready(Duration::from_secs(60)).await?;
    tokio::time::sleep(Duration::from_secs(settle)).await;
    println!("[探针] 就位");

    let spec =
        |keys: HeldKeys, mouse: Option<MouseButton>, turn: Option<Turn>, ticks: u32| InputSpec {
            keys,
            mouse,
            turn,
            ticks,
        };

    press(&module, 1, "W 1 秒", spec(keys(&["w"]), None, None, 20)).await;
    press(
        &module,
        2,
        "W+Ctrl 1 秒（疾跑）",
        spec(keys(&["w", "ctrl"]), None, None, 20),
    )
    .await;
    press(
        &module,
        3,
        "右转 90°（纯转向）",
        spec(
            HeldKeys::default(),
            None,
            Some(Turn {
                yaw: 90.0,
                pitch: 0.0,
            }),
            1,
        ),
    )
    .await;
    press(
        &module,
        4,
        "A 0.5 秒（左平移）",
        spec(keys(&["a"]), None, None, 10),
    )
    .await;
    press(
        &module,
        5,
        "D 0.5 秒（右平移）",
        spec(keys(&["d"]), None, None, 10),
    )
    .await;
    press(
        &module,
        55,
        "点空格（跳一下）",
        spec(keys(&["space"]), None, None, 1),
    )
    .await;

    let _ = module.execute(DoorCommand::SelectSlot(0)).await;
    press(
        &module,
        6,
        "低头 90° 按住左键最多 5 秒（钻镐挖脚下，碎即松）",
        spec(
            HeldKeys::default(),
            Some(MouseButton::Left),
            Some(Turn {
                yaw: 0.0,
                pitch: 90.0,
            }),
            100,
        ),
    )
    .await;

    let _ = module.execute(DoorCommand::SelectSlot(1)).await;
    press(
        &module,
        7,
        "空格+按住右键 1 秒（原地垫方块）",
        spec(keys(&["space"]), Some(MouseButton::Right), None, 20),
    )
    .await;

    let _ = module.execute(DoorCommand::SelectSlot(2)).await;
    press(
        &module,
        8,
        "抬头看天，点右键（金苹果：点一下不该吃掉）",
        spec(
            HeldKeys::default(),
            Some(MouseButton::Right),
            Some(Turn {
                yaw: 0.0,
                pitch: -180.0,
            }),
            1,
        ),
    )
    .await;
    press(
        &module,
        9,
        "按住右键 2 秒（金苹果：吃完一个）",
        spec(HeldKeys::default(), Some(MouseButton::Right), None, 40),
    )
    .await;

    let _ = module.execute(DoorCommand::SelectSlot(0)).await;
    press(
        &module,
        10,
        "下看 35°，点左键（面前 2 格的猪应挨一下）",
        spec(
            HeldKeys::default(),
            Some(MouseButton::Left),
            Some(Turn {
                yaw: 0.0,
                pitch: 125.0,
            }),
            1,
        ),
    )
    .await;

    println!("[探针] 结束");
    module.stop("探针结束").await
}
