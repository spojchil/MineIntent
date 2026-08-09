//! 三种通用 wire 协议的适配层。

mod anthropic_messages;
mod openai_chat;
mod openai_responses;
mod transport;

pub(crate) use transport::{HttpModel, Protocol, WireLogPolicy};

#[cfg(test)]
mod tests;
