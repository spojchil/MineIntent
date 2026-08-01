//! B 独占的应用事件 journal。
//!
//! 这里持久化的是 `mineintent.event.v1` 应用信封；它不是 backend 事实流使用的
//! `mineintent.minecraft.backend-event.v2`。

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

pub const JOURNAL_FILE_MODE: u32 = 0o600;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum JournalEventProtocol {
    #[default]
    #[serde(rename = "mineintent.event.v1")]
    V1,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct JournalEvent<T = serde_json::Value> {
    pub protocol: JournalEventProtocol,
    pub id: Uuid,
    #[serde(rename = "type")]
    pub event_type: String,
    pub occurred_at: String,
    pub world_id: String,
    pub session_id: String,
    pub payload: T,
}

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("cannot resolve the journal path: {0}")]
    CurrentDirectory(#[source] io::Error),
    #[error("journal event is not serializable: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("JsonlEventJournal requires an active Tokio runtime")]
    RuntimeUnavailable,
    #[error("journal writer stopped")]
    WriterStopped,
    #[error("journal write failed: {0}")]
    Write(String),
}

#[derive(Clone)]
pub struct JsonlEventJournal {
    world_id: String,
    session_id: String,
    writer: mpsc::Sender<WriteRequest>,
}

impl JsonlEventJournal {
    pub fn new(
        file: impl AsRef<Path>,
        world_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<Self, JournalError> {
        let file = absolute_path(file.as_ref())?;
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| JournalError::RuntimeUnavailable)?;
        let (writer, receiver) = mpsc::channel(64);
        runtime.spawn(writer_loop(file, receiver));
        Ok(Self {
            world_id: world_id.into(),
            session_id: session_id.into(),
            writer,
        })
    }

    pub async fn append<T>(
        &self,
        event_type: impl Into<String>,
        payload: T,
    ) -> Result<JournalEvent<T>, JournalError>
    where
        T: Serialize,
    {
        let event = JournalEvent {
            protocol: JournalEventProtocol::V1,
            id: Uuid::new_v4(),
            event_type: event_type.into(),
            occurred_at: current_timestamp(),
            world_id: self.world_id.clone(),
            session_id: self.session_id.clone(),
            payload,
        };
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');

        let (acknowledge, completion) = oneshot::channel();
        self.writer
            .send(WriteRequest::Append { line, acknowledge })
            .await
            .map_err(|_| JournalError::WriterStopped)?;
        completion
            .await
            .map_err(|_| JournalError::WriterStopped)??;
        Ok(event)
    }

    /// Waits until every write queued before this barrier has completed.
    pub async fn flush(&self) -> Result<(), JournalError> {
        let (acknowledge, completion) = oneshot::channel();
        self.writer
            .send(WriteRequest::Flush { acknowledge })
            .await
            .map_err(|_| JournalError::WriterStopped)?;
        completion.await.map_err(|_| JournalError::WriterStopped)?
    }
}

enum WriteRequest {
    Append {
        line: Vec<u8>,
        acknowledge: oneshot::Sender<Result<(), JournalError>>,
    },
    Flush {
        acknowledge: oneshot::Sender<Result<(), JournalError>>,
    },
}

async fn writer_loop(file: PathBuf, mut receiver: mpsc::Receiver<WriteRequest>) {
    let mut failure: Option<String> = None;
    while let Some(request) = receiver.recv().await {
        match request {
            WriteRequest::Append { line, acknowledge } => {
                let result = match &failure {
                    Some(message) => Err(JournalError::Write(message.clone())),
                    None => match write_line(file.clone(), line).await {
                        Ok(()) => Ok(()),
                        Err(error) => {
                            failure = Some(error.clone());
                            Err(JournalError::Write(error))
                        }
                    },
                };
                let _ = acknowledge.send(result);
            }
            WriteRequest::Flush { acknowledge } => {
                let result = failure
                    .as_ref()
                    .map_or(Ok(()), |message| Err(JournalError::Write(message.clone())));
                let _ = acknowledge.send(result);
            }
        }
    }
}

async fn write_line(file: PathBuf, line: Vec<u8>) -> Result<(), String> {
    tokio::task::spawn_blocking(move || append_line(&file, &line))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

fn append_line(file: &Path, line: &[u8]) -> io::Result<()> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut options = OpenOptions::new();
    options.create(true).append(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(JOURNAL_FILE_MODE);
    }
    let mut output = options.open(file)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        output.set_permissions(fs::Permissions::from_mode(JOURNAL_FILE_MODE))?;
    }
    output.write_all(line)?;
    output.flush()
}

fn absolute_path(file: &Path) -> Result<PathBuf, JournalError> {
    if file.is_absolute() {
        Ok(file.to_owned())
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(file))
            .map_err(JournalError::CurrentDirectory)
    }
}

fn current_timestamp() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = elapsed.as_secs();
    let seconds_in_day = seconds % 86_400;
    let (year, month, day) = civil_date((seconds / 86_400) as i64);
    let hour = seconds_in_day / 3_600;
    let minute = seconds_in_day % 3_600 / 60;
    let second = seconds_in_day % 60;
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        elapsed.subsec_millis()
    )
}

fn civil_date(days_since_epoch: i64) -> (i64, i64, i64) {
    let shifted = days_since_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}
