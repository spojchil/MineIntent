use mineintent_middle::information::{
    contracts::{
        InformationConnectionState, InformationInterfaceId, InformationScopeDependency,
        InformationScopeSnapshot, InformationSourceKind, InformationTraceRecord,
    },
    scope::{scope_changed, InformationScopeSource, MutableInformationScopeSource},
    trace::{
        InMemoryInformationTrace, InformationTraceError, InformationTraceSink,
        NOOP_INFORMATION_TRACE,
    },
};

fn scope() -> InformationScopeSnapshot {
    InformationScopeSnapshot {
        process_session_id: "process-1".to_owned(),
        connection_state: InformationConnectionState::Play,
        connection_epoch: 4,
        world_id: Some("world-a".to_owned()),
        dimension: Some("minecraft:overworld".to_owned()),
        ui_revision: 8,
        screen_instance_id: Some("screen-a".to_owned()),
        screen_revision: Some(2),
        captured_at: "2026-08-01T00:00:00Z".to_owned(),
    }
}

#[test]
fn characterization_mutable_scope_source_captures_owned_snapshots() {
    let source = MutableInformationScopeSource::new(scope());
    let before = source.capture();
    let mut next = before.clone();
    next.connection_epoch = 5;
    next.world_id = Some("world-b".to_owned());
    source.update(next.clone());

    assert_eq!(before.connection_epoch, 4);
    assert_eq!(before.world_id.as_deref(), Some("world-a"));
    assert_eq!(source.capture(), next);

    fn accepts_scope_source(_source: &dyn InformationScopeSource) {}
    accepts_scope_source(&source);
}

#[test]
fn characterization_scope_changed_compares_only_declared_dependencies() {
    let before = scope();
    let mut after = before.clone();
    after.captured_at = "2026-08-01T00:01:00Z".to_owned();
    assert!(!scope_changed(&before, &after, &[]));

    after.process_session_id = "process-2".to_owned();
    assert!(scope_changed(&before, &after, &[]));

    let dependency_changes: [(
        InformationScopeDependency,
        fn(&mut InformationScopeSnapshot),
    ); 5] = [
        (
            InformationScopeDependency::Connection,
            |scope: &mut InformationScopeSnapshot| scope.connection_epoch += 1,
        ),
        (InformationScopeDependency::World, |scope| {
            scope.world_id = Some("world-b".to_owned())
        }),
        (InformationScopeDependency::Dimension, |scope| {
            scope.dimension = Some("minecraft:the_nether".to_owned())
        }),
        (InformationScopeDependency::Ui, |scope| {
            scope.ui_revision += 1
        }),
        (InformationScopeDependency::Screen, |scope| {
            scope.screen_revision = Some(3)
        }),
    ];
    for (dependency, mutate) in dependency_changes {
        let mut changed = before.clone();
        mutate(&mut changed);
        assert!(scope_changed(&before, &changed, &[dependency]));
    }

    let mut connection_state = before.clone();
    connection_state.connection_state = InformationConnectionState::Connecting;
    assert!(scope_changed(
        &before,
        &connection_state,
        &[InformationScopeDependency::Connection]
    ));
    let mut screen_instance = before.clone();
    screen_instance.screen_instance_id = Some("screen-b".to_owned());
    assert!(scope_changed(
        &before,
        &screen_instance,
        &[InformationScopeDependency::Screen]
    ));

    let mut unrelated = before.clone();
    unrelated.dimension = Some("minecraft:the_end".to_owned());
    assert!(!scope_changed(
        &before,
        &unrelated,
        &[InformationScopeDependency::World]
    ));
}

#[test]
fn characterization_trace_keeps_newest_records_in_append_order() {
    assert!(matches!(
        InMemoryInformationTrace::new(0),
        Err(InformationTraceError::InvalidCapacity)
    ));
    let trace = InMemoryInformationTrace::new(2).expect("positive trace capacity");
    trace.append(record("read-1"));
    trace.append(record("read-2"));
    trace.append(record("read-3"));

    let mut records = trace.records();
    assert_eq!(
        records
            .iter()
            .map(|record| record.read_id.as_str())
            .collect::<Vec<_>>(),
        ["read-2", "read-3"]
    );
    records[0].fields.push("mutated-copy".to_owned());
    assert_eq!(trace.records()[0].fields, ["health"]);
}

#[test]
fn contract_trace_sink_is_object_safe_and_noop_discards_records() {
    fn accepts_trace_sink(_sink: &dyn InformationTraceSink) {}
    accepts_trace_sink(&NOOP_INFORMATION_TRACE);
    NOOP_INFORMATION_TRACE.append(record("discarded"));
}

fn record(read_id: &str) -> InformationTraceRecord {
    InformationTraceRecord {
        read_id: read_id.to_owned(),
        interface_id: InformationInterfaceId::CurrentStatus,
        fields: vec!["health".to_owned()],
        source_kind: InformationSourceKind::ClientState,
        source_revision: 1,
        evidence_ids: vec!["evidence-1".to_owned()],
        correlation_id: "correlation-1".to_owned(),
        observed_at: "2026-08-01T00:00:00Z".to_owned(),
    }
}
