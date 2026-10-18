//! Typed identifiers. Every platform identifier is a server-generated UUID; distinct
//! types stop a tenant id from being passed where an application id is expected.

typed_id!(
    /// Platform customer: the top-level security, ownership and usage boundary.
    TenantId
);
typed_id!(
    /// Product / application owned by exactly one tenant.
    ApplicationId
);
typed_id!(
    /// One SANDBOX / TEST / PRODUCTION environment of an application.
    EnvironmentId
);
typed_id!(
    /// Machine identity bound to one application environment.
    ServiceAccountId
);
typed_id!(
    /// API credential owned by one service account.
    CredentialId
);
typed_id!(
    /// Communication principal (participant / entity) inside one application environment.
    PrincipalId
);
