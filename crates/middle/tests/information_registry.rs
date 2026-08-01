use std::{collections::BTreeMap, sync::Arc};

use mineintent_contracts::minecraft::{BoxFuture, OperationControl};
use mineintent_middle::information::{
    contracts::{
        InformationAudience, InformationCatalogEntryAvailability, InformationFieldDefinition,
        InformationInterfaceId, InformationPrecision, InformationProvider,
        InformationProviderContext, InformationProviderDefinition, InformationProviderError,
        InformationProviderLimits, InformationProviderPagination, InformationProviderSelectors,
        InformationScopeDependency, InformationSourceKind, InformationValueSchema,
        InformationValueSchemaError, ProviderAvailability, ProviderReadRequest, ProviderReadResult,
    },
    registry::{InformationDefinitionError, InformationRegistry, InformationRegistryError},
};
use serde_json::Value;

struct NumberSchema;

impl InformationValueSchema for NumberSchema {
    fn parse(&self, value: Value) -> Result<Value, InformationValueSchemaError> {
        if value.is_number() {
            Ok(value)
        } else {
            Err(InformationValueSchemaError {
                message: "expected number".to_owned(),
            })
        }
    }
}

struct FakeProvider {
    definition: InformationProviderDefinition,
}

impl InformationProvider for FakeProvider {
    fn definition(&self) -> &InformationProviderDefinition {
        &self.definition
    }

    fn availability(&self, _context: &InformationProviderContext<'_>) -> ProviderAvailability {
        ProviderAvailability {
            overall: InformationCatalogEntryAvailability::Available,
            information_revision: 1,
            fields: BTreeMap::new(),
        }
    }

    fn read<'a>(
        &'a self,
        _context: InformationProviderContext<'a>,
        _request: ProviderReadRequest,
        _control: OperationControl,
    ) -> BoxFuture<'a, Result<ProviderReadResult, InformationProviderError>> {
        Box::pin(async {
            Err(InformationProviderError::Failed {
                message: "unused registry fixture read".to_owned(),
            })
        })
    }
}

fn field_definition() -> InformationFieldDefinition {
    InformationFieldDefinition {
        description: "Visible value".to_owned(),
        value_schema: Arc::new(NumberSchema),
        value_type: "number".to_owned(),
        unit: None,
        precision: InformationPrecision::Displayed,
        source_kinds: vec![InformationSourceKind::ClientState],
        requires: None,
        notes: None,
    }
}

fn definition(id: InformationInterfaceId) -> InformationProviderDefinition {
    InformationProviderDefinition {
        id,
        description: format!("Provider {}", interface_name(id)),
        schema_revision: format!("{}:1", interface_name(id)),
        audiences: vec![InformationAudience::Participant],
        fields: BTreeMap::from([("value".to_owned(), field_definition())]),
        scope_dependencies: vec![InformationScopeDependency::Connection],
        selectors: None,
        pagination: None,
        limits: InformationProviderLimits {
            max_fields_per_read: 1,
            max_result_bytes: 1_024,
            timeout_ms: 100,
        },
    }
}

fn provider(id: InformationInterfaceId) -> Arc<dyn InformationProvider> {
    Arc::new(FakeProvider {
        definition: definition(id),
    })
}

fn provider_from_definition(
    definition: InformationProviderDefinition,
) -> Arc<dyn InformationProvider> {
    Arc::new(FakeProvider { definition })
}

#[test]
fn registry_is_deterministic_sealed_and_rejects_duplicate_providers() {
    let left = InformationRegistry::new();
    left.register(provider(InformationInterfaceId::CurrentStatus))
        .expect("first provider should register");
    left.register(provider(InformationInterfaceId::UiContext))
        .expect("second provider should register");
    left.seal("1.21.1").expect("left registry should seal");

    let right = InformationRegistry::new();
    right
        .register(provider(InformationInterfaceId::UiContext))
        .expect("first provider should register");
    right
        .register(provider(InformationInterfaceId::CurrentStatus))
        .expect("second provider should register");
    right.seal("1.21.1").expect("right registry should seal");

    assert_eq!(
        left.catalog_revision().expect("left revision"),
        right.catalog_revision().expect("right revision")
    );
    assert_eq!(
        left.descriptors()
            .expect("sealed descriptors")
            .into_iter()
            .map(|descriptor| descriptor.id)
            .collect::<Vec<_>>(),
        vec![
            InformationInterfaceId::CurrentStatus,
            InformationInterfaceId::UiContext
        ]
    );
    assert_eq!(
        left.register(provider(InformationInterfaceId::F3Information)),
        Err(InformationRegistryError::Sealed)
    );

    let duplicate = InformationRegistry::new();
    duplicate
        .register(provider(InformationInterfaceId::CurrentStatus))
        .expect("first duplicate fixture should register");
    assert_eq!(
        duplicate.register(provider(InformationInterfaceId::CurrentStatus)),
        Err(InformationRegistryError::DuplicateProvider {
            id: "current_status".to_owned()
        })
    );
    assert!(matches!(
        duplicate.provider(InformationInterfaceId::CurrentStatus),
        Err(InformationRegistryError::NotSealed)
    ));
}

#[test]
fn rust_contract_catalog_revision_locks_the_ts_canonical_sha256_input() {
    let registry = InformationRegistry::new();
    registry
        .register(provider(InformationInterfaceId::UiContext))
        .expect("ui provider should register");
    registry
        .register(provider(InformationInterfaceId::CurrentStatus))
        .expect("status provider should register");
    registry.seal("1.21.1").expect("registry should seal");

    assert_eq!(
        registry.catalog_revision().expect("catalog revision"),
        "catalog:1.21.1:5c2f95176291633f"
    );

    let mut sortable = definition(InformationInterfaceId::F3Information);
    sortable.audiences = vec![
        InformationAudience::Operator,
        InformationAudience::Participant,
    ];
    sortable
        .fields
        .insert("alpha".to_owned(), field_definition());
    sortable
        .fields
        .insert("zeta".to_owned(), field_definition());
    let sorted_registry = InformationRegistry::new();
    sorted_registry
        .register(provider_from_definition(sortable))
        .expect("sortable provider should register");
    sorted_registry
        .seal("1.21.1")
        .expect("sortable registry should seal");
    let descriptor = sorted_registry
        .descriptors()
        .expect("descriptors")
        .remove(0);
    assert_eq!(descriptor.field_ids, ["alpha", "value", "zeta"]);
    assert_eq!(
        descriptor.audiences,
        [
            InformationAudience::Operator,
            InformationAudience::Participant
        ]
    );
}

#[test]
fn rust_contract_definition_validation_returns_structured_errors() {
    fn assert_invalid(
        definition: InformationProviderDefinition,
        expected: InformationDefinitionError,
    ) {
        let registry = InformationRegistry::new();
        assert_eq!(
            registry.register(provider_from_definition(definition)),
            Err(InformationRegistryError::InvalidDefinition {
                provider: "current_status".to_owned(),
                reason: expected,
            })
        );
    }

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    invalid.description = "  ".to_owned();
    assert_invalid(invalid, InformationDefinitionError::MissingDescription);

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    invalid.schema_revision.clear();
    assert_invalid(invalid, InformationDefinitionError::MissingSchemaRevision);

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    invalid.audiences.clear();
    assert_invalid(invalid, InformationDefinitionError::InvalidAudiences);

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    invalid.scope_dependencies = vec![
        InformationScopeDependency::Connection,
        InformationScopeDependency::Connection,
    ];
    assert_invalid(
        invalid,
        InformationDefinitionError::InvalidScopeDependencies,
    );

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    invalid.fields.clear();
    assert_invalid(invalid, InformationDefinitionError::MissingFields);

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    let field = invalid.fields.remove("value").expect("fixture field");
    invalid.fields.insert(" ".to_owned(), field);
    assert_invalid(invalid, InformationDefinitionError::EmptyFieldId);

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    invalid
        .fields
        .get_mut("value")
        .expect("fixture field")
        .description
        .clear();
    assert_invalid(
        invalid,
        InformationDefinitionError::MissingFieldDescription {
            field: "value".to_owned(),
        },
    );

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    invalid
        .fields
        .get_mut("value")
        .expect("fixture field")
        .value_type
        .clear();
    assert_invalid(
        invalid,
        InformationDefinitionError::MissingFieldValueType {
            field: "value".to_owned(),
        },
    );

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    invalid
        .fields
        .get_mut("value")
        .expect("fixture field")
        .source_kinds
        .clear();
    assert_invalid(
        invalid,
        InformationDefinitionError::InvalidFieldSourceKinds {
            field: "value".to_owned(),
        },
    );

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    invalid.limits.max_fields_per_read = 0;
    assert_invalid(invalid, InformationDefinitionError::InvalidFieldLimit);

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    invalid.limits.max_result_bytes = 0;
    assert_invalid(invalid, InformationDefinitionError::InvalidByteLimit);

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    invalid.limits.timeout_ms = 0;
    assert_invalid(invalid, InformationDefinitionError::InvalidTimeout);

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    invalid.pagination = Some(InformationProviderPagination {
        default_limit: 2,
        max_limit: 1,
    });
    assert_invalid(invalid, InformationDefinitionError::InvalidPaginationLimits);

    let mut invalid = definition(InformationInterfaceId::CurrentStatus);
    invalid.selectors = Some(InformationProviderSelectors {
        required: true,
        accepts_kinds: Vec::new(),
    });
    assert_invalid(invalid, InformationDefinitionError::InvalidSelectorKinds);
}

#[test]
fn rust_contract_registry_reads_require_seal_and_lifecycle_errors_do_not_panic() {
    let empty = InformationRegistry::new();
    assert_eq!(
        empty.seal("1.21.1"),
        Err(InformationRegistryError::NoProviders)
    );

    let registry = InformationRegistry::new();
    registry
        .register(provider(InformationInterfaceId::CurrentStatus))
        .expect("provider should register");
    assert!(matches!(
        registry.descriptors(),
        Err(InformationRegistryError::NotSealed)
    ));
    assert_eq!(
        registry.catalog_revision(),
        Err(InformationRegistryError::NotSealed)
    );
    assert_eq!(
        registry.target_minecraft_version(),
        Err(InformationRegistryError::NotSealed)
    );
    assert_eq!(
        registry.seal("   "),
        Err(InformationRegistryError::TargetMinecraftVersionRequired)
    );
    registry.seal(" 1.21.1 ").expect("trim is validation-only");
    assert_eq!(
        registry.target_minecraft_version().expect("stored version"),
        " 1.21.1 "
    );
    assert_eq!(
        registry.seal("1.21.1"),
        Err(InformationRegistryError::AlreadySealed)
    );
}

fn interface_name(id: InformationInterfaceId) -> &'static str {
    match id {
        InformationInterfaceId::UiContext => "ui_context",
        InformationInterfaceId::CurrentStatus => "current_status",
        InformationInterfaceId::F3Information => "f3_information",
        _ => "fixture_interface",
    }
}
