//! 运行时输入信箱。
//!
//! 投递时机由状态机边界而非模型角色决定：
//! - `NextModelRequest` 是引导输入，在下次调用模型前排空；如果模型即将结束，
//!   则改为再调用一次模型。
//! - `WhenIdle` 是后续输入，仅在本次运行原本将要结束时排空。

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::run::RequestBoundaryKind;
use crate::types::TranscriptItem;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    NextModelRequest,
    WhenIdle,
}

/// 一个原子信箱信封，其中的项目保持顺序并整体投递。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MailboxInput {
    pub delivery: Delivery,
    pub items: Vec<TranscriptItem>,
}

impl MailboxInput {
    pub fn next_model_request(items: Vec<TranscriptItem>) -> Self {
        Self {
            delivery: Delivery::NextModelRequest,
            items,
        }
    }

    pub fn when_idle(items: Vec<TranscriptItem>) -> Self {
        Self {
            delivery: Delivery::WhenIdle,
            items,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MailboxRejectedReason {
    Idle,
    Closing,
    Stopping,
    /// 信封中的记录段包含孤立或未闭合的工具调用/结果。
    InvalidInput,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MailboxRejected {
    pub reason: MailboxRejectedReason,
    pub input: MailboxInput,
}

impl std::fmt::Display for MailboxRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "mailbox input rejected: {:?}", self.reason)
    }
}

impl std::error::Error for MailboxRejected {}

#[derive(Default)]
pub(crate) struct Mailbox {
    next_request: VecDeque<MailboxInput>,
    when_idle: VecDeque<MailboxInput>,
}

impl Mailbox {
    pub(crate) fn push(&mut self, input: MailboxInput) {
        match input.delivery {
            Delivery::NextModelRequest => self.next_request.push_back(input),
            Delivery::WhenIdle => self.when_idle.push_back(input),
        }
    }

    /// 排空操作是信箱的线性化点：检查与清除在一次操作中完成。
    /// 在完成边界，引导输入优先；每类输入内部保持先进先出。
    pub(crate) fn drain(&mut self, boundary: RequestBoundaryKind) -> Vec<TranscriptItem> {
        let mut drained = Self::flatten(&mut self.next_request);
        if boundary == RequestBoundaryKind::BeforeCompletion {
            drained.extend(Self::flatten(&mut self.when_idle));
        }
        drained
    }

    pub(crate) fn drain_all(&mut self) -> Vec<MailboxInput> {
        let mut pending = self.next_request.drain(..).collect::<Vec<_>>();
        pending.extend(self.when_idle.drain(..));
        pending
    }

    fn flatten(queue: &mut VecDeque<MailboxInput>) -> Vec<TranscriptItem> {
        queue.drain(..).flat_map(|input| input.items).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::InputMessage;

    fn item(role: &str, text: &str) -> TranscriptItem {
        InputMessage::text(role, text).into()
    }

    #[test]
    fn next_request_and_idle_delivery_have_distinct_boundaries() {
        let mut mailbox = Mailbox::default();
        mailbox.push(MailboxInput::when_idle(vec![item("operator", "later")]));
        mailbox.push(MailboxInput::next_model_request(vec![item(
            "developer",
            "now",
        )]));

        let first = mailbox.drain(RequestBoundaryKind::BeforeModelRequest);
        assert_eq!(first, vec![item("developer", "now")]);

        let final_boundary = mailbox.drain(RequestBoundaryKind::BeforeCompletion);
        assert_eq!(final_boundary, vec![item("operator", "later")]);
    }
}
