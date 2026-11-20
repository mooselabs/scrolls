//! Role-claim verification supplied by the embedding host.
use crate::model::{Role, TenantId};
use anyhow::{Result, ensure};

/// A host verifies an opaque credential against this exact request. A verifier
/// should be scoped to the destination store and check all applicable fields.
pub struct VerificationRequest<'a> {
    pub tenant: &'a TenantId,
    pub actor: &'a str,
    pub claimed_role: Role,
    pub op_id: &'a str,
    pub request_fingerprint: &'a str,
    pub dry_run: bool,
    pub label_move: Option<LabelMove<'a>>,
}

pub struct LabelMove<'a> {
    /// The canonical record ID after resolving any alias.
    pub record_id: &'a str,
    pub label: &'a str,
    /// The supplied destination, which Scrolls requires to be an exact revision.
    pub target_revision: &'a str,
}

/// Refusal reasons contain no caller data and are safe to return in errors.
#[derive(Debug)]
pub enum CredentialRefusal {
    InvalidCredential,
    ActorMismatch,
    RoleDenied,
    OperationDenied,
    Unavailable,
}

impl std::fmt::Display for CredentialRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidCredential => "invalid credential",
            Self::ActorMismatch => "credential does not match the actor",
            Self::RoleDenied => "credential does not grant the claimed role",
            Self::OperationDenied => "credential does not authorize this operation",
            Self::Unavailable => "credential verification unavailable",
        })
    }
}

pub trait CredentialVerifier {
    /// Confirm the claimed role for this operation or return a refusal reason.
    fn verify(
        &self,
        request: &VerificationRequest<'_>,
        credential: &[u8],
    ) -> Result<(), CredentialRefusal>;
}

pub(crate) enum Authorization<'a> {
    Cooperative(Role),
    Credentialed {
        tenant: &'a TenantId,
        actor: &'a str,
        role: Role,
        verifier: &'a dyn CredentialVerifier,
        credential: &'a [u8],
    },
}

impl Authorization<'_> {
    pub(crate) fn requires_verification(&self) -> bool {
        matches!(self, Self::Credentialed { .. })
    }

    pub(crate) fn role(
        &self,
        op_id: &str,
        request_fingerprint: &str,
        dry_run: bool,
        label_move: Option<LabelMove<'_>>,
    ) -> Result<Role> {
        match *self {
            Self::Cooperative(role) => Ok(role),
            Self::Credentialed {
                tenant,
                actor,
                role,
                verifier,
                credential,
            } => {
                let decision = verifier.verify(
                    &VerificationRequest {
                        tenant,
                        actor,
                        claimed_role: role,
                        op_id,
                        request_fingerprint,
                        dry_run,
                        label_move,
                    },
                    credential,
                );
                if let Err(reason) = decision {
                    anyhow::bail!("credential_refused: {reason}");
                }
                Ok(role)
            }
        }
    }
}

pub(crate) fn check_controller(
    role: Role,
    verifier: Option<&dyn CredentialVerifier>,
    moves_label: bool,
) -> Result<()> {
    if role == Role::Controller {
        ensure!(
            verifier.is_some(),
            "controller_verifier_required: controller role requires a host credential verifier"
        );
        ensure!(
            moves_label,
            "controller_operation_refused: controller role may only move labels"
        );
    }
    Ok(())
}
