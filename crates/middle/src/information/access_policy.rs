use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, RwLock},
};

use thiserror::Error;

use super::{
    contracts::{
        InformationAllowedInterfaces, InformationGrant, InformationProviderDescriptor,
        InformationScopeSnapshot,
    },
    ref_store::parse_rfc3339_millis,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InformationAuthorizationOperation {
    Catalog,
    Help,
    Read,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InformationAuthorizationDenialReason {
    AudienceDenied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InformationAuthorizationResult {
    Allowed,
    Denied {
        reason: InformationAuthorizationDenialReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InformationAccessPolicyStoreOperation {
    Put,
    Revoke,
    Resolve,
}

impl fmt::Display for InformationAccessPolicyStoreOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Put => formatter.write_str("put"),
            Self::Revoke => formatter.write_str("revoke"),
            Self::Resolve => formatter.write_str("resolve"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum InformationAccessPolicyError {
    #[error("information access policy lock was poisoned during {operation}")]
    LockPoisoned {
        operation: InformationAccessPolicyStoreOperation,
    },
}

pub trait InformationAccessPolicy: Send + Sync {
    fn resolve(
        &self,
        grant_id: &str,
        principal_id: &str,
    ) -> Result<Option<InformationGrant>, InformationAccessPolicyError>;

    fn authorize(
        &self,
        grant: &InformationGrant,
        provider: &InformationProviderDescriptor,
        operation: InformationAuthorizationOperation,
        fields: &[String],
        scope: &InformationScopeSnapshot,
    ) -> InformationAuthorizationResult;
}

#[derive(Clone, Default)]
pub struct InMemoryInformationAccessPolicy {
    grants: Arc<RwLock<HashMap<String, InformationGrant>>>,
}

impl InMemoryInformationAccessPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, grant: &InformationGrant) -> Result<(), InformationAccessPolicyError> {
        let mut grants =
            self.grants
                .write()
                .map_err(|_| InformationAccessPolicyError::LockPoisoned {
                    operation: InformationAccessPolicyStoreOperation::Put,
                })?;
        grants.insert(grant.id.clone(), grant.clone());
        Ok(())
    }

    pub fn revoke(&self, grant_id: &str) -> Result<(), InformationAccessPolicyError> {
        let mut grants =
            self.grants
                .write()
                .map_err(|_| InformationAccessPolicyError::LockPoisoned {
                    operation: InformationAccessPolicyStoreOperation::Revoke,
                })?;
        grants.remove(grant_id);
        Ok(())
    }
}

impl InformationAccessPolicy for InMemoryInformationAccessPolicy {
    fn resolve(
        &self,
        grant_id: &str,
        principal_id: &str,
    ) -> Result<Option<InformationGrant>, InformationAccessPolicyError> {
        let grants =
            self.grants
                .read()
                .map_err(|_| InformationAccessPolicyError::LockPoisoned {
                    operation: InformationAccessPolicyStoreOperation::Resolve,
                })?;
        Ok(grants
            .get(grant_id)
            .filter(|grant| grant.principal_id == principal_id)
            .cloned())
    }

    fn authorize(
        &self,
        grant: &InformationGrant,
        provider: &InformationProviderDescriptor,
        _operation: InformationAuthorizationOperation,
        fields: &[String],
        scope: &InformationScopeSnapshot,
    ) -> InformationAuthorizationResult {
        if is_expired(grant.valid_until.as_deref(), &scope.captured_at)
            || !provider.audiences.contains(&grant.audience)
            || !includes_interface(&grant.allowed_interfaces, provider.id)
            || grant
                .connection_epoch
                .is_some_and(|epoch| epoch != scope.connection_epoch)
            || grant
                .world_id
                .as_ref()
                .is_some_and(|world| Some(world) != scope.world_id.as_ref())
            || grant
                .screen_instance_id
                .as_ref()
                .is_some_and(|screen| Some(screen) != scope.screen_instance_id.as_ref())
            || grant
                .allowed_fields
                .as_ref()
                .and_then(|allowed| allowed.get(&provider.id))
                .is_some_and(|allowed| fields.iter().any(|field| !allowed.contains(field)))
        {
            return InformationAuthorizationResult::Denied {
                reason: InformationAuthorizationDenialReason::AudienceDenied,
            };
        }
        InformationAuthorizationResult::Allowed
    }
}

fn is_expired(valid_until: Option<&str>, now: &str) -> bool {
    valid_until
        .and_then(parse_rfc3339_millis)
        .zip(parse_rfc3339_millis(now))
        .is_some_and(|(valid_until, now)| valid_until <= now)
}

fn includes_interface(
    allowed: &InformationAllowedInterfaces,
    interface_id: super::contracts::InformationInterfaceId,
) -> bool {
    match allowed {
        InformationAllowedInterfaces::All(_) => true,
        InformationAllowedInterfaces::Interfaces(interfaces) => interfaces.contains(&interface_id),
    }
}
