use std::{collections::VecDeque, sync::Mutex};

use thiserror::Error;

use super::contracts::InformationTraceRecord;

pub const DEFAULT_INFORMATION_TRACE_CAPACITY: usize = 1_024;

pub trait InformationTraceSink: Send + Sync {
    fn append(&self, record: InformationTraceRecord);
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum InformationTraceError {
    #[error("information trace capacity must be positive")]
    InvalidCapacity,
}

pub struct InMemoryInformationTrace {
    max_records: usize,
    records: Mutex<VecDeque<InformationTraceRecord>>,
}

impl InMemoryInformationTrace {
    pub fn new(max_records: usize) -> Result<Self, InformationTraceError> {
        if max_records < 1 {
            return Err(InformationTraceError::InvalidCapacity);
        }
        Ok(Self {
            max_records,
            records: Mutex::new(VecDeque::with_capacity(max_records)),
        })
    }

    pub fn records(&self) -> Vec<InformationTraceRecord> {
        match self.records.lock() {
            Ok(records) => records.iter().cloned().collect(),
            Err(poisoned) => poisoned.into_inner().iter().cloned().collect(),
        }
    }
}

impl Default for InMemoryInformationTrace {
    fn default() -> Self {
        Self {
            max_records: DEFAULT_INFORMATION_TRACE_CAPACITY,
            records: Mutex::new(VecDeque::with_capacity(DEFAULT_INFORMATION_TRACE_CAPACITY)),
        }
    }
}

impl InformationTraceSink for InMemoryInformationTrace {
    fn append(&self, record: InformationTraceRecord) {
        let mut records = match self.records.lock() {
            Ok(records) => records,
            Err(poisoned) => poisoned.into_inner(),
        };
        records.push_back(record);
        if records.len() > self.max_records {
            records.pop_front();
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopInformationTrace;

impl InformationTraceSink for NoopInformationTrace {
    fn append(&self, _record: InformationTraceRecord) {}
}

pub const NOOP_INFORMATION_TRACE: NoopInformationTrace = NoopInformationTrace;
