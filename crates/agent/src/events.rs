//! 分级、可过滤且不携带正文的生命周期事件。
//!
//! 核心事件只暴露数量、标识符、名称、边界和错误类别，不复制提示词、工具参数、
//! 工具结果、服务商响应正文或凭据。需要正文诊断时，应在模型/工具适配器中单独实现
//! 显式启用、截断和脱敏的日志。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use crate::mailbox::Delivery;
use crate::run::RequestBoundaryKind;
use crate::types::{AgentErrorKind, ModelUsage, RunId, ToolBatchId, ToolCallId, ToolName};

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum EventLevel {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

/// `Off` 既是总开关，其余值表示允许的最大详细程度。
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum LevelFilter {
    Off = 0,
    Error = 1,
    Warn = 2,
    #[default]
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl LevelFilter {
    pub const fn allows(self, level: EventLevel) -> bool {
        level as u8 <= self as u8
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Error,
            2 => Self::Warn,
            3 => Self::Info,
            4 => Self::Debug,
            5 => Self::Trace,
            _ => Self::Off,
        }
    }
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum EventCategory {
    Lifecycle = 0,
    Mailbox = 1,
    Boundary = 2,
    Model = 3,
    Tools = 4,
    Compaction = 5,
}

impl EventCategory {
    const ALL_MASK: u8 = (1 << 6) - 1;

    const fn mask(self) -> u8 {
        1 << self as u8
    }
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
    RunStarted,
    MailboxEnqueued,
    BoundaryDrained,
    CompletionSealed,
    ModelRequestStarted,
    ModelRequestFinished,
    ToolBatchStarted,
    ToolBatchFinished,
    CompactionStarted,
    CompactionFinished,
    RunCompleted,
    RunStopped,
    RunFailed,
}

impl EventKind {
    pub const fn metadata(self) -> EventMetadata {
        let (level, category) = match self {
            Self::RunFailed => (EventLevel::Error, EventCategory::Lifecycle),
            Self::RunStarted | Self::RunCompleted | Self::RunStopped => {
                (EventLevel::Info, EventCategory::Lifecycle)
            }
            Self::ModelRequestStarted | Self::ModelRequestFinished => {
                (EventLevel::Debug, EventCategory::Model)
            }
            Self::ToolBatchStarted | Self::ToolBatchFinished => {
                (EventLevel::Debug, EventCategory::Tools)
            }
            Self::CompactionStarted | Self::CompactionFinished => {
                (EventLevel::Debug, EventCategory::Compaction)
            }
            Self::MailboxEnqueued => (EventLevel::Trace, EventCategory::Mailbox),
            Self::BoundaryDrained | Self::CompletionSealed => {
                (EventLevel::Trace, EventCategory::Boundary)
            }
        };
        EventMetadata {
            kind: self,
            level,
            category,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventMetadata {
    pub kind: EventKind,
    pub level: EventLevel,
    pub category: EventCategory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStage {
    Boundary,
    Model,
    Tools,
    Compaction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolCallSummary {
    pub id: ToolCallId,
    pub name: ToolName,
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    RunStarted {
        sequence: u64,
        run_id: RunId,
        prior_transcript_items: usize,
    },
    MailboxEnqueued {
        sequence: u64,
        run_id: RunId,
        delivery: Delivery,
        item_count: usize,
    },
    BoundaryDrained {
        sequence: u64,
        run_id: RunId,
        kind: RequestBoundaryKind,
        item_count: usize,
    },
    CompletionSealed {
        sequence: u64,
        run_id: RunId,
    },
    ModelRequestStarted {
        sequence: u64,
        run_id: RunId,
        request_index: u64,
        transcript_items: usize,
        function_tools: usize,
    },
    ModelRequestFinished {
        sequence: u64,
        run_id: RunId,
        request_index: u64,
        local_tool_calls: usize,
        duration_ms: u64,
        usage: Option<ModelUsage>,
    },
    ToolBatchStarted {
        sequence: u64,
        run_id: RunId,
        batch_id: ToolBatchId,
        calls: Vec<ToolCallSummary>,
    },
    ToolBatchFinished {
        sequence: u64,
        run_id: RunId,
        batch_id: ToolBatchId,
        result_count: usize,
        error_count: usize,
        duration_ms: u64,
    },
    CompactionStarted {
        sequence: u64,
        run_id: RunId,
        transcript_items: usize,
        estimated_bytes: usize,
    },
    CompactionFinished {
        sequence: u64,
        run_id: RunId,
        transcript_items: usize,
        duration_ms: u64,
    },
    RunCompleted {
        sequence: u64,
        run_id: RunId,
        model_requests: u64,
        tool_batches: u64,
        usage: Option<ModelUsage>,
    },
    RunStopped {
        sequence: u64,
        run_id: RunId,
    },
    RunFailed {
        sequence: u64,
        run_id: RunId,
        stage: RunStage,
        /// 完整错误摘要可能含服务商正文，因此事件只携带稳定错误类别。
        error_kind: AgentErrorKind,
    },
}

impl AgentEvent {
    /// 在 session 的语义事件发生点分配；Observer 可用它还原并发写出的真实顺序。
    pub const fn sequence(&self) -> u64 {
        match self {
            Self::RunStarted { sequence, .. }
            | Self::MailboxEnqueued { sequence, .. }
            | Self::BoundaryDrained { sequence, .. }
            | Self::CompletionSealed { sequence, .. }
            | Self::ModelRequestStarted { sequence, .. }
            | Self::ModelRequestFinished { sequence, .. }
            | Self::ToolBatchStarted { sequence, .. }
            | Self::ToolBatchFinished { sequence, .. }
            | Self::CompactionStarted { sequence, .. }
            | Self::CompactionFinished { sequence, .. }
            | Self::RunCompleted { sequence, .. }
            | Self::RunStopped { sequence, .. }
            | Self::RunFailed { sequence, .. } => *sequence,
        }
    }

    pub const fn kind(&self) -> EventKind {
        match self {
            Self::RunStarted { .. } => EventKind::RunStarted,
            Self::MailboxEnqueued { .. } => EventKind::MailboxEnqueued,
            Self::BoundaryDrained { .. } => EventKind::BoundaryDrained,
            Self::CompletionSealed { .. } => EventKind::CompletionSealed,
            Self::ModelRequestStarted { .. } => EventKind::ModelRequestStarted,
            Self::ModelRequestFinished { .. } => EventKind::ModelRequestFinished,
            Self::ToolBatchStarted { .. } => EventKind::ToolBatchStarted,
            Self::ToolBatchFinished { .. } => EventKind::ToolBatchFinished,
            Self::CompactionStarted { .. } => EventKind::CompactionStarted,
            Self::CompactionFinished { .. } => EventKind::CompactionFinished,
            Self::RunCompleted { .. } => EventKind::RunCompleted,
            Self::RunStopped { .. } => EventKind::RunStopped,
            Self::RunFailed { .. } => EventKind::RunFailed,
        }
    }

    pub const fn metadata(&self) -> EventMetadata {
        self.kind().metadata()
    }
}

pub trait Observer: Send + Sync {
    /// 在事件构造前调用，可用于级别、类别和动态开关过滤。
    fn enabled(&self, _metadata: EventMetadata) -> bool {
        true
    }

    fn observe(&self, event: &AgentEvent);
}

/// 用原子级别和类别掩码包装任意 Observer，可在运行期间无锁切换。
pub struct FilteredObserver {
    inner: Arc<dyn Observer>,
    level: AtomicU8,
    category_mask: AtomicU8,
}

impl FilteredObserver {
    pub fn new(inner: Arc<dyn Observer>, level: LevelFilter) -> Self {
        Self {
            inner,
            level: AtomicU8::new(level as u8),
            category_mask: AtomicU8::new(EventCategory::ALL_MASK),
        }
    }

    pub fn set_level(&self, level: LevelFilter) {
        self.level.store(level as u8, Ordering::Relaxed);
    }

    /// 空切片表示关闭全部类别。
    pub fn set_categories(&self, categories: &[EventCategory]) {
        let mask = categories
            .iter()
            .fold(0, |mask, category| mask | category.mask());
        self.category_mask.store(mask, Ordering::Relaxed);
    }
}

impl Observer for FilteredObserver {
    fn enabled(&self, metadata: EventMetadata) -> bool {
        let level = LevelFilter::from_u8(self.level.load(Ordering::Relaxed));
        let categories = self.category_mask.load(Ordering::Relaxed);
        level.allows(metadata.level)
            && categories & metadata.category.mask() != 0
            && self.inner.enabled(metadata)
    }

    fn observe(&self, event: &AgentEvent) {
        self.inner.observe(event);
    }
}

#[derive(Default)]
pub struct NoopObserver;

impl Observer for NoopObserver {
    fn enabled(&self, _metadata: EventMetadata) -> bool {
        false
    }

    fn observe(&self, _event: &AgentEvent) {}
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct RecordingObserver {
        kinds: Mutex<Vec<EventKind>>,
    }

    impl Observer for RecordingObserver {
        fn observe(&self, event: &AgentEvent) {
            self.kinds.lock().unwrap().push(event.kind());
        }
    }

    #[test]
    fn level_filter_orders_severity_and_supports_off() {
        assert!(!LevelFilter::Off.allows(EventLevel::Error));
        assert!(LevelFilter::Info.allows(EventLevel::Error));
        assert!(LevelFilter::Info.allows(EventLevel::Info));
        assert!(!LevelFilter::Info.allows(EventLevel::Debug));
        assert!(LevelFilter::Trace.allows(EventLevel::Trace));
    }

    #[test]
    fn filtered_observer_switches_level_and_categories_dynamically() {
        let recording = Arc::new(RecordingObserver::default());
        let filtered = FilteredObserver::new(recording, LevelFilter::Off);
        let model = EventKind::ModelRequestStarted.metadata();
        assert!(!filtered.enabled(model));

        filtered.set_level(LevelFilter::Debug);
        assert!(filtered.enabled(model));
        assert!(!filtered.enabled(EventKind::BoundaryDrained.metadata()));

        filtered.set_categories(&[EventCategory::Tools]);
        assert!(!filtered.enabled(model));
        assert!(filtered.enabled(EventKind::ToolBatchStarted.metadata()));
    }
}
