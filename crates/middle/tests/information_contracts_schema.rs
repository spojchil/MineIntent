use mineintent_middle::information::contracts::{
    parse_information_catalog_request, parse_information_query_request,
    parse_information_selector_ref, InformationProvider, InformationQueryRequest,
    InformationReferenceIssueError, InformationReferenceIssueRequest, InformationReferenceIssuer,
    InformationSelectorRef, InformationValueSchema, INFORMATION_AUDIENCES,
    INFORMATION_AVAILABILITIES, INFORMATION_ERROR_CODES, INFORMATION_INTERFACE_IDS,
    INFORMATION_SCOPE_DEPENDENCIES, INFORMATION_SOURCE_KINDS,
};
use serde_json::json;

#[test]
fn information_request_schemas_are_strict_and_versioned() {
    assert!(parse_information_catalog_request(r#"{"operation":"list_interfaces"}"#).is_ok());
    assert!(parse_information_catalog_request(
        r#"{"operation":"list_interfaces","audience":"operator"}"#
    )
    .is_err());
    assert!(parse_information_query_request(
        r#"{"interfaceId":"current_status","operation":"read","schemaRevision":"status:1","fields":["health"]}"#
    )
    .is_ok());
    assert!(parse_information_query_request(
        r#"{"interfaceId":"current_status","operation":"read","schemaRevision":"status:1","fields":["health"],"worldId":"forged-world"}"#
    )
    .is_err());
    assert!(parse_information_query_request(
        r#"{"interfaceId":"not-an-interface","operation":"help"}"#
    )
    .is_err());
}

#[test]
fn exported_v1_enumerations_are_complete_and_strict() {
    assert_eq!(INFORMATION_INTERFACE_IDS.len(), 17);
    assert_eq!(INFORMATION_AUDIENCES.len(), 3);
    assert_eq!(INFORMATION_SOURCE_KINDS.len(), 8);
    assert_eq!(INFORMATION_AVAILABILITIES.len(), 9);
    assert_eq!(INFORMATION_SCOPE_DEPENDENCIES.len(), 5);
    assert_eq!(INFORMATION_ERROR_CODES.len(), 11);
    assert!(serde_json::from_str::<
        mineintent_middle::information::contracts::InformationInterfaceId,
    >(r#""not-an-interface""#)
    .is_err());
}

#[test]
fn query_parser_preserves_unicode_and_optional_fields() {
    let request = parse_information_query_request(
        r#"{"interfaceId":"chat_information","operation":"help","search":"矿物😀","fields":[]}"#,
    )
    .expect("Unicode help request should parse");
    let InformationQueryRequest::Help(help) = request else {
        panic!("expected help request");
    };
    assert_eq!(help.search.as_deref(), Some("矿物😀"));
    assert_eq!(help.fields, Some(Vec::new()));

    let minimal =
        parse_information_query_request(r#"{"interfaceId":"chat_information","operation":"help"}"#)
            .expect("optional fields may be omitted");
    let serialized = serde_json::to_value(minimal).expect("request should serialize");
    assert_eq!(
        serialized,
        json!({"interfaceId":"chat_information","operation":"help"})
    );
    assert!(parse_information_query_request(
        r#"{"interfaceId":"chat_information","operation":"help","search":null}"#
    )
    .is_err());
}

#[test]
fn selector_and_page_constraints_match_the_zod_schema() {
    let valid = r#"{
        "interfaceId":"inventory_information",
        "operation":"read",
        "schemaRevision":"inventory:1",
        "fields":["slots"],
        "selector":{
            "protocol":"mineintent.information-selector-ref.v1",
            "id":"selector-0000001",
            "interfaceId":"inventory_information",
            "connectionEpoch":0,
            "basedOnInformationRevision":4,
            "validUntil":"2026-08-01T12:34:56.123Z"
        },
        "page":{"cursor":"cursor-0000000001","limit":10000}
    }"#;
    assert!(parse_information_query_request(valid).is_ok());
    assert!(
        parse_information_query_request(&valid.replace("\"limit\":10000", "\"limit\":1.0")).is_ok()
    );

    for invalid in [
        valid.replace("selector-0000001", "short"),
        valid.replace("2026-08-01T12:34:56.123Z", "2026-02-30T12:34:56Z"),
        valid.replace("\"limit\":10000", "\"limit\":10001"),
        valid.replace(
            "\"connectionEpoch\":0",
            "\"connectionEpoch\":9007199254740992",
        ),
    ] {
        assert!(
            parse_information_query_request(&invalid).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn exported_selector_parser_is_strict_and_versioned() {
    let selector = r#"{
        "protocol":"mineintent.information-selector-ref.v1",
        "id":"selector-0000001",
        "interfaceId":"current_status",
        "connectionEpoch":1.0,
        "basedOnInformationRevision":0
    }"#;
    assert!(parse_information_selector_ref(selector).is_ok());
    assert!(parse_information_selector_ref(&selector.replace(
        "mineintent.information-selector-ref.v1",
        "mineintent.information-selector-ref.v2"
    ))
    .is_err());
    assert!(parse_information_selector_ref(&selector.replace(
        "\"basedOnInformationRevision\":0",
        "\"basedOnInformationRevision\":0,\"extra\":true"
    ))
    .is_err());
}

#[test]
fn provider_spi_traits_are_object_safe() {
    fn accepts_trait_objects(
        _provider: Option<&dyn InformationProvider>,
        _schema: Option<&dyn InformationValueSchema>,
        _issuer: Option<&dyn InformationReferenceIssuer>,
    ) {
    }

    accepts_trait_objects(None, None, None);

    fn issue_through_object_safe_port(
        issuer: &dyn InformationReferenceIssuer,
        request: InformationReferenceIssueRequest,
    ) -> Result<InformationSelectorRef, InformationReferenceIssueError> {
        issuer.issue(request)
    }
    let _fallible_port_signature = issue_through_object_safe_port;
}

#[test]
fn representative_wire_dtos_reject_unknown_fields() {
    let result = serde_json::from_str::<
        mineintent_middle::information::contracts::InformationReadSource,
    >(
        r#"{"kind":"client_state","adapterRevision":"a:1","sourceRevision":1,"acquisition":"immediate_client_state","providerRevision":2}"#,
    );
    assert!(result.is_err());
}
