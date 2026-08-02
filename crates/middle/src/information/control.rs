//! Per-operation cancellation and deadline controls used by the information runtime.
//!
//! The contracts crate deliberately owns only the signal traits.  This module supplies a small
//! child-token implementation so the runtime can forward cancellation and enforce provider and
//! tool-session deadlines without holding any information-store lock across an await.

use std::{
    future::pending,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use mineintent_contracts::minecraft::{BoxFuture, CancellationSignal, Deadline, OperationControl};
use tokio::sync::Notify;

pub(crate) struct RuntimeCancellation {
    triggered: AtomicBool,
    notify: Notify,
}

impl RuntimeCancellation {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            triggered: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    pub(crate) fn trigger(&self) {
        if !self.triggered.swap(true, Ordering::SeqCst) {
            // `notify_one` preserves a permit for the check/register race; `notify_waiters`
            // wakes all already-registered waiters.  Both are harmless on repeated triggers.
            self.notify.notify_waiters();
            self.notify.notify_one();
        }
    }
}

impl CancellationSignal for RuntimeCancellation {
    fn is_cancelled(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }

    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            loop {
                if self.is_cancelled() {
                    return;
                }
                self.notify.notified().await;
            }
        })
    }
}

pub(crate) struct RuntimeDeadline {
    triggered: AtomicBool,
    notify: Notify,
}

impl RuntimeDeadline {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            triggered: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    pub(crate) fn trigger(&self) {
        if !self.triggered.swap(true, Ordering::SeqCst) {
            self.notify.notify_waiters();
            self.notify.notify_one();
        }
    }
}

impl Deadline for RuntimeDeadline {
    fn has_elapsed(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }

    fn elapsed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            loop {
                if self.has_elapsed() {
                    return;
                }
                self.notify.notified().await;
            }
        })
    }
}

pub(crate) fn child_operation_control() -> (
    OperationControl,
    Arc<RuntimeCancellation>,
    Arc<RuntimeDeadline>,
) {
    let cancellation = RuntimeCancellation::new();
    let deadline = RuntimeDeadline::new();
    let control = OperationControl::new(
        Arc::clone(&cancellation) as Arc<dyn CancellationSignal>,
        Some(Arc::clone(&deadline) as Arc<dyn Deadline>),
    );
    (control, cancellation, deadline)
}

pub(crate) fn pending_unit<'a>() -> BoxFuture<'a, ()> {
    Box::pin(pending())
}
