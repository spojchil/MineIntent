//! 控制台日志：进程的 stdout 与 stderr 照常显示在终端，同时写进
//! `<数据目录>/logs/latest.log`，和启动器、原版客户端的 `logs/` 一样。
//!
//! 在文件描述符这一层分流，所以 Azalea/Bevy 自己的日志与 panic 信息也进得去，
//! 不用改任何一处打印。上次运行的 `latest.log` 按它的修改时间改名留存，最多留
//! [`KEEP`] 份。
//!
//! 日志里有聊天原文和服务器地址，只在本机；外传前自己看一眼。

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use filedescriptor::{FileDescriptor, Pipe, StdioDescriptor};

/// 留几份旧日志。
const KEEP: usize = 20;

/// 分流中的控制台。[`ConsoleLog::finish`] 把终端还原，并等分流线程把剩下的写完。
///
/// stdout 与 stderr 接到**同一根**管道、由一个线程转写：两路各用一根管道时，
/// 两个线程谁先被调度说不准，文件里的先后会和实际打印的先后对不上（实测过：
/// 退出时的错误跑到了它前面几行之前）。代价是终端上两路合成一路，都从原 stdout 出。
pub struct ConsoleLog {
    originals: Vec<(StdioDescriptor, FileDescriptor)>,
    pump: Option<JoinHandle<()>>,
    path: PathBuf,
}

impl ConsoleLog {
    /// 开始分流。失败时返回原因，调用方照常运行、只是没有日志文件。
    pub fn start(logs_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(logs_dir)
            .map_err(|error| format!("建日志目录 {} 失败：{error}", logs_dir.display()))?;
        let path = logs_dir.join("latest.log");
        rotate(logs_dir, &path);
        let mut file = File::create(&path)
            .map_err(|error| format!("建日志文件 {} 失败：{error}", path.display()))?;
        let pipe = Pipe::new().map_err(|error| format!("建管道失败：{error}"))?;
        let mut console = Self {
            originals: Vec::new(),
            pump: None,
            path,
        };
        for which in [StdioDescriptor::Stdout, StdioDescriptor::Stderr] {
            match FileDescriptor::redirect_stdio(&pipe.write, which) {
                Ok(original) => console.originals.push((which, original)),
                Err(error) => {
                    // 已经接管的那一路要还原，不能留半截。
                    console.finish();
                    return Err(format!("接管 {which:?} 失败：{error}"));
                }
            }
        }
        // 写端只留在标准流上：这一份不放掉，还原后管道就永远读不到结尾。
        drop(pipe.write);
        let mut terminal = match console.originals[0].1.try_clone() {
            Ok(terminal) => terminal,
            Err(error) => {
                console.finish();
                return Err(format!("复制原 stdout 失败：{error}"));
            }
        };
        let mut read = pipe.read;
        let pump = std::thread::Builder::new()
            .name("console-log".to_owned())
            .spawn(move || {
                let mut buffer = [0u8; 8192];
                loop {
                    match read.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let _ = terminal.write_all(&buffer[..n]);
                            let _ = file.write_all(&buffer[..n]);
                        }
                    }
                }
            });
        match pump {
            Ok(pump) => console.pump = Some(pump),
            Err(error) => {
                console.finish();
                return Err(format!("起分流线程失败：{error}"));
            }
        }
        Ok(console)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 还原终端：管道写端随之关闭，分流线程读到结尾、写完后退出。
    pub fn finish(self) {
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        for (which, original) in &self.originals {
            let _ = FileDescriptor::redirect_stdio(original, *which);
        }
        if let Some(pump) = self.pump {
            let _ = pump.join();
        }
    }
}

/// 上次的 `latest.log` 按修改时间改名，再删掉超出 [`KEEP`] 份的最旧几份。
fn rotate(dir: &Path, latest: &Path) {
    if let Ok(meta) = std::fs::metadata(latest) {
        let at = meta
            .modified()
            .unwrap_or_else(|_| std::time::SystemTime::now());
        let stem = crate::paths::timestamp_name(at);
        let mut target = dir.join(format!("{stem}.log"));
        let mut n = 1;
        while target.exists() {
            target = dir.join(format!("{stem}-{n}.log"));
            n += 1;
        }
        let _ = std::fs::rename(latest, target);
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut old: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "log")
                && path.file_name().is_some_and(|name| name != "latest.log")
        })
        .collect();
    // 文件名就是时间，按名排序即按时间排序。
    old.sort();
    let excess = old.len().saturating_sub(KEEP);
    for path in old.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_keeps_the_newest_logs() {
        let dir = std::env::temp_dir().join(format!("mineintent-rotate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for day in 1..=(KEEP + 3) {
            std::fs::write(dir.join(format!("2026-01-{day:02}_00-00-00.log")), "").unwrap();
        }
        let latest = dir.join("latest.log");
        std::fs::write(&latest, "上一次").unwrap();
        rotate(&dir, &latest);
        assert!(!latest.exists());
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), KEEP);
        // 最旧的几份删掉了，刚改名的上一次还在。
        assert!(!names.contains(&"2026-01-01_00-00-00.log".to_owned()));
        assert!(names.iter().any(|name| !name.starts_with("2026-01-")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
