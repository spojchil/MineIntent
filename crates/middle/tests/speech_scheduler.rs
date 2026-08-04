//! `scheduler_rate_limits_and_preserves_segment_order` and
//! `scheduler_stop_cancels_queued_speech_before_it_is_sent` map one-to-one to the final two tests
//! in `speech.test.ts` (oracle lines 76 and 91). All waits use Tokio paused time for #125.
//! Remaining tests are explicitly additional scheduler contract tests.

use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::Duration,
};

use mineintent_middle::speech::{
    SegmentChatError, SpeechEvent, SpeechEventHandler, SpeechRequest, SpeechScheduleError,
    SpeechScheduler, SpeechSchedulerBuildError, SpeechSchedulerOptions, SpeechTransport,
};
use tokio::time::{advance, Instant};

#[tokio::test(start_paused = true)]
async fn scheduler_rate_limits_and_preserves_segment_order() {
    let transport = RecordingTransport::default();
    let events = EventLog::default();
    let scheduler = SpeechScheduler::new(
        transport.clone(),
        options(5, Duration::from_millis(5), &events),
    )
    .unwrap();

    let segments = scheduler
        .schedule(request("reply", "我去拿一些木头回来"))
        .unwrap();
    assert_eq!(segments, 2);
    assert_eq!(
        events.snapshot(),
        vec![SpeechEvent::Scheduled {
            request_id: "reply".to_owned(),
            segments: 2,
        }]
    );
    assert!(
        transport.snapshot().is_empty(),
        "the first segment is async"
    );

    drive_worker().await;
    let first = transport.snapshot();
    assert_eq!(first.len(), 1);
    advance(Duration::from_millis(4)).await;
    drive_worker().await;
    assert_eq!(transport.snapshot().len(), 1);
    advance(Duration::from_millis(1)).await;
    drive_worker().await;

    let sent = transport.snapshot();
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent.iter()
            .map(|record| record.text.as_str())
            .collect::<String>(),
        "我去拿一些木头回来"
    );
    assert!(sent[1].at.duration_since(sent[0].at) >= Duration::from_millis(5));
    assert_eq!(
        events.snapshot(),
        vec![
            SpeechEvent::Scheduled {
                request_id: "reply".to_owned(),
                segments: 2,
            },
            SpeechEvent::Sent {
                request_id: "reply".to_owned(),
                segment: 0,
                text: sent[0].text.clone(),
            },
            SpeechEvent::Sent {
                request_id: "reply".to_owned(),
                segment: 1,
                text: sent[1].text.clone(),
            },
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn scheduler_stop_cancels_queued_speech_before_it_is_sent() {
    let transport = RecordingTransport::default();
    let events = EventLog::default();
    let scheduler =
        SpeechScheduler::new(transport.clone(), options(256, Duration::ZERO, &events)).unwrap();

    scheduler
        .schedule(request("reply", "这里风景不错"))
        .unwrap();
    scheduler.stop_with_reason("test_stopped");
    assert_eq!(
        events.snapshot(),
        vec![
            SpeechEvent::Scheduled {
                request_id: "reply".to_owned(),
                segments: 1,
            },
            SpeechEvent::Cancelled {
                request_id: "reply".to_owned(),
                reason: "test_stopped".to_owned(),
            },
        ]
    );

    advance(Duration::from_secs(1)).await;
    drive_worker().await;
    assert!(transport.snapshot().is_empty());
    assert_eq!(events.snapshot().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn additional_concurrent_stop_does_not_cancel_an_already_claimed_delivery() {
    let transport = BlockingTransport::default();
    let events = EventLog::default();
    let scheduler = Arc::new(
        SpeechScheduler::new(transport.clone(), options(256, Duration::ZERO, &events)).unwrap(),
    );
    scheduler
        .schedule(request("claimed", "已经开始发送"))
        .unwrap();

    let stopping_scheduler = Arc::clone(&scheduler);
    let stopping_transport = transport.clone();
    let stopper = thread::spawn(move || {
        stopping_transport.wait_until_entered();
        stopping_scheduler.stop_with_reason("concurrent_stop");
        stopping_transport.release();
    });

    drive_worker().await;
    stopper.join().unwrap();
    assert_eq!(transport.snapshot(), vec!["已经开始发送"]);
    assert_eq!(
        events.snapshot(),
        vec![
            SpeechEvent::Scheduled {
                request_id: "claimed".to_owned(),
                segments: 1,
            },
            SpeechEvent::Sent {
                request_id: "claimed".to_owned(),
                segment: 0,
                text: "已经开始发送".to_owned(),
            },
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn additional_concurrent_stop_after_segment_claim_cancels_remaining_segments() {
    let transport = BlockingTransport::default();
    let events = EventLog::default();
    let scheduler = Arc::new(
        SpeechScheduler::new(transport.clone(), options(2, Duration::ZERO, &events)).unwrap(),
    );
    assert_eq!(scheduler.schedule(request("multi", "甲乙丙丁")).unwrap(), 2);

    let stopping_scheduler = Arc::clone(&scheduler);
    let stopping_transport = transport.clone();
    let stopper = thread::spawn(move || {
        stopping_transport.wait_until_entered();
        stopping_scheduler.stop_with_reason("concurrent_stop");
        stopping_transport.release();
    });

    drive_worker().await;
    stopper.join().unwrap();
    assert_eq!(transport.snapshot(), vec!["甲乙"]);
    assert_eq!(
        events.snapshot(),
        vec![
            SpeechEvent::Scheduled {
                request_id: "multi".to_owned(),
                segments: 2,
            },
            SpeechEvent::Sent {
                request_id: "multi".to_owned(),
                segment: 0,
                text: "甲乙".to_owned(),
            },
            SpeechEvent::Cancelled {
                request_id: "multi".to_owned(),
                reason: "concurrent_stop".to_owned(),
            },
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn additional_transport_failure_discards_current_request_and_continues_fifo() {
    let transport = RecordingTransport::with_failures([true, false]);
    let events = EventLog::default();
    let scheduler = SpeechScheduler::new(
        transport.clone(),
        options(256, Duration::from_millis(50), &events),
    )
    .unwrap();
    scheduler.schedule(request("bad", "第一条")).unwrap();
    scheduler.schedule(request("good", "第二条")).unwrap();

    drive_worker().await;

    let calls = transport.snapshot();
    assert_eq!(
        calls
            .iter()
            .map(|record| record.text.as_str())
            .collect::<Vec<_>>(),
        vec!["第一条", "第二条"]
    );
    assert_eq!(
        events.snapshot(),
        vec![
            SpeechEvent::Scheduled {
                request_id: "bad".to_owned(),
                segments: 1,
            },
            SpeechEvent::Scheduled {
                request_id: "good".to_owned(),
                segments: 1,
            },
            SpeechEvent::Failed {
                request_id: "bad".to_owned(),
                reason: "scripted transport failure".to_owned(),
            },
            SpeechEvent::Sent {
                request_id: "good".to_owned(),
                segment: 0,
                text: "第二条".to_owned(),
            },
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn additional_transport_panic_becomes_failed_and_worker_continues_fifo() {
    let transport = RecordingTransport::with_panics([true, false]);
    let events = EventLog::default();
    let scheduler =
        SpeechScheduler::new(transport.clone(), options(256, Duration::ZERO, &events)).unwrap();
    scheduler.schedule(request("panic", "第一条")).unwrap();
    scheduler.schedule(request("good", "第二条")).unwrap();

    drive_worker().await;

    assert_eq!(
        transport
            .snapshot()
            .iter()
            .map(|record| record.text.as_str())
            .collect::<Vec<_>>(),
        vec!["第二条"]
    );
    assert_eq!(
        events.snapshot(),
        vec![
            SpeechEvent::Scheduled {
                request_id: "panic".to_owned(),
                segments: 1,
            },
            SpeechEvent::Scheduled {
                request_id: "good".to_owned(),
                segments: 1,
            },
            SpeechEvent::Failed {
                request_id: "panic".to_owned(),
                reason: "scripted transport panic".to_owned(),
            },
            SpeechEvent::Sent {
                request_id: "good".to_owned(),
                segment: 0,
                text: "第二条".to_owned(),
            },
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn additional_duplicate_id_is_rejected_only_while_request_is_queued() {
    let transport = RecordingTransport::default();
    let events = EventLog::default();
    let scheduler =
        SpeechScheduler::new(transport.clone(), options(256, Duration::ZERO, &events)).unwrap();
    scheduler.schedule(request("reply", "第一次")).unwrap();
    assert_eq!(
        scheduler.schedule(request("reply", "重复")),
        Err(SpeechScheduleError::DuplicateRequest {
            request_id: "reply".to_owned(),
        })
    );
    assert_eq!(
        scheduler.schedule(request("reply", "\0")),
        Err(SpeechScheduleError::DuplicateRequest {
            request_id: "reply".to_owned(),
        })
    );

    drive_worker().await;
    scheduler.schedule(request("reply", "第二次")).unwrap();
    drive_worker().await;
    assert_eq!(
        transport
            .snapshot()
            .iter()
            .map(|record| record.text.as_str())
            .collect::<Vec<_>>(),
        vec!["第一次", "第二次"]
    );
}

#[tokio::test(start_paused = true)]
async fn additional_stop_cancels_every_queued_request_in_fifo_order() {
    let transport = RecordingTransport::default();
    let events = EventLog::default();
    let scheduler = SpeechScheduler::new(
        transport.clone(),
        options(256, Duration::from_secs(10), &events),
    )
    .unwrap();
    for id in ["one", "two", "three"] {
        scheduler.schedule(request(id, id)).unwrap();
    }

    scheduler.stop();
    let events = events.snapshot();
    assert_eq!(events.len(), 6);
    assert!(matches!(events[0], SpeechEvent::Scheduled { .. }));
    assert!(matches!(events[1], SpeechEvent::Scheduled { .. }));
    assert!(matches!(events[2], SpeechEvent::Scheduled { .. }));
    let cancelled: Vec<_> = events[3..]
        .iter()
        .map(|event| match event {
            SpeechEvent::Cancelled { request_id, reason } => {
                assert_eq!(reason, "scheduler_stopped");
                request_id.as_str()
            }
            other => panic!("expected cancellation, got {other:?}"),
        })
        .collect();
    assert_eq!(cancelled, vec!["one", "two", "three"]);
    advance(Duration::from_secs(60)).await;
    drive_worker().await;
    assert!(transport.snapshot().is_empty());
}

#[test]
fn additional_constructor_validation_is_structured_and_does_not_require_panics() {
    let invalid = SpeechScheduler::new(
        RecordingTransport::default(),
        SpeechSchedulerOptions {
            max_segment_length: 0,
            ..SpeechSchedulerOptions::default()
        },
    );
    assert!(matches!(
        invalid,
        Err(SpeechSchedulerBuildError::InvalidMaxSegmentLength)
    ));

    let without_runtime = SpeechScheduler::new(
        RecordingTransport::default(),
        SpeechSchedulerOptions::default(),
    );
    assert!(matches!(
        without_runtime,
        Err(SpeechSchedulerBuildError::RuntimeUnavailable)
    ));
}

#[tokio::test(start_paused = true)]
async fn additional_unrepresentable_interval_is_a_structured_build_error() {
    let result = SpeechScheduler::new(
        RecordingTransport::default(),
        SpeechSchedulerOptions {
            minimum_interval: Duration::MAX,
            ..SpeechSchedulerOptions::default()
        },
    );
    assert!(matches!(
        result,
        Err(SpeechSchedulerBuildError::InvalidMinimumInterval)
    ));
}

#[tokio::test(start_paused = true)]
async fn additional_request_validation_is_structured_and_emits_nothing() {
    let events = EventLog::default();
    let scheduler = SpeechScheduler::new(
        RecordingTransport::default(),
        options(256, Duration::ZERO, &events),
    )
    .unwrap();
    assert_eq!(
        scheduler.schedule(request("", "hello")),
        Err(SpeechScheduleError::InvalidRequest)
    );
    assert_eq!(
        scheduler.schedule(request("blank", " \r\n\t ")),
        Err(SpeechScheduleError::InvalidRequest)
    );
    assert_eq!(
        scheduler.schedule(request("nul", " \0 ")),
        Err(SpeechScheduleError::Segment(
            SegmentChatError::EmptyAfterNormalization
        ))
    );
    assert!(events.snapshot().is_empty());
}

#[derive(Clone, Default)]
struct RecordingTransport {
    state: Arc<Mutex<TransportState>>,
}

#[derive(Default)]
struct TransportState {
    calls: Vec<SentRecord>,
    failures: VecDeque<bool>,
    panics: VecDeque<bool>,
}

#[derive(Clone)]
struct SentRecord {
    at: Instant,
    text: String,
}

impl RecordingTransport {
    fn with_failures(failures: impl IntoIterator<Item = bool>) -> Self {
        Self {
            state: Arc::new(Mutex::new(TransportState {
                calls: Vec::new(),
                failures: failures.into_iter().collect(),
                panics: VecDeque::new(),
            })),
        }
    }

    fn with_panics(panics: impl IntoIterator<Item = bool>) -> Self {
        Self {
            state: Arc::new(Mutex::new(TransportState {
                calls: Vec::new(),
                failures: VecDeque::new(),
                panics: panics.into_iter().collect(),
            })),
        }
    }

    fn snapshot(&self) -> Vec<SentRecord> {
        self.state.lock().unwrap().calls.clone()
    }
}

#[derive(Debug)]
struct TestTransportError;

impl fmt::Display for TestTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("scripted transport failure")
    }
}

impl std::error::Error for TestTransportError {}

impl SpeechTransport for RecordingTransport {
    type Error = TestTransportError;

    fn send(&self, message: &str) -> Result<(), Self::Error> {
        let (should_panic, should_fail) = {
            let mut state = self.state.lock().unwrap();
            (
                state.panics.pop_front().unwrap_or(false),
                state.failures.pop_front().unwrap_or(false),
            )
        };
        if should_panic {
            panic!("scripted transport panic");
        }
        let mut state = self.state.lock().unwrap();
        state.calls.push(SentRecord {
            at: Instant::now(),
            text: message.to_owned(),
        });
        if should_fail {
            Err(TestTransportError)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Default)]
struct BlockingTransport {
    state: Arc<(Mutex<BlockingTransportState>, Condvar)>,
}

#[derive(Default)]
struct BlockingTransportState {
    entered: bool,
    released: bool,
    calls: Vec<String>,
}

impl BlockingTransport {
    fn wait_until_entered(&self) {
        let (state, changed) = &*self.state;
        let mut state = state.lock().unwrap();
        while !state.entered {
            state = changed.wait(state).unwrap();
        }
    }

    fn release(&self) {
        let (state, changed) = &*self.state;
        let mut state = state.lock().unwrap();
        state.released = true;
        changed.notify_all();
    }

    fn snapshot(&self) -> Vec<String> {
        self.state.0.lock().unwrap().calls.clone()
    }
}

impl SpeechTransport for BlockingTransport {
    type Error = TestTransportError;

    fn send(&self, message: &str) -> Result<(), Self::Error> {
        let (state, changed) = &*self.state;
        let mut state = state.lock().unwrap();
        state.entered = true;
        changed.notify_all();
        while !state.released {
            state = changed.wait(state).unwrap();
        }
        state.calls.push(message.to_owned());
        Ok(())
    }
}

#[derive(Clone, Default)]
struct EventLog(Arc<Mutex<Vec<SpeechEvent>>>);

impl EventLog {
    fn handler(&self) -> SpeechEventHandler {
        let events = Arc::clone(&self.0);
        Arc::new(move |event| events.lock().unwrap().push(event))
    }

    fn snapshot(&self) -> Vec<SpeechEvent> {
        self.0.lock().unwrap().clone()
    }
}

fn options(
    max_segment_length: usize,
    minimum_interval: Duration,
    events: &EventLog,
) -> SpeechSchedulerOptions {
    SpeechSchedulerOptions {
        max_segment_length,
        minimum_interval,
        on_event: events.handler(),
    }
}

fn request(id: &str, text: &str) -> SpeechRequest {
    SpeechRequest {
        id: id.to_owned(),
        text: text.to_owned(),
    }
}

async fn drive_worker() {
    for _ in 0..12 {
        tokio::task::yield_now().await;
    }
}
