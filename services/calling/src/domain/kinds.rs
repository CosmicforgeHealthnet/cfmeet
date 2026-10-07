//! Persisted platform enumerations. Values match the `CHECK` constraints in
//! `migration/src/`.

text_enum!(
    /// Trust classification of a tenant. FIRST_PARTY never selects a separate API or
    /// code path; it only gates explicitly declared trusted scopes.
    TenantClassification {
        FirstParty = "FIRST_PARTY",
        External = "EXTERNAL",
    }
);

text_enum!(TenantStatus {
    Active = "ACTIVE",
    Suspended = "SUSPENDED",
});

text_enum!(ApplicationStatus {
    Active = "ACTIVE",
    Suspended = "SUSPENDED",
});

text_enum!(EnvironmentKind {
    Sandbox = "SANDBOX",
    Test = "TEST",
    Production = "PRODUCTION",
});

text_enum!(EnvironmentStatus {
    Active = "ACTIVE",
    Suspended = "SUSPENDED",
});

text_enum!(
    /// REVOKED is terminal (database-enforced).
    ServiceAccountStatus {
        Active = "ACTIVE",
        Suspended = "SUSPENDED",
        Revoked = "REVOKED",
    }
);

text_enum!(
    /// Persisted credential status. REVOKED is terminal (database-enforced). Expiry and
    /// rotation supersession are time-based and reported as an effective state instead.
    CredentialStatus {
        Active = "ACTIVE",
        Revoked = "REVOKED",
    }
);

text_enum!(
    /// Category of communication principal. CF Meet attaches no consumer business
    /// meaning to any category.
    PrincipalKind {
        NativeCfUser = "NATIVE_CF_USER",
        ExternalApplicationPrincipal = "EXTERNAL_APPLICATION_PRINCIPAL",
        EnterpriseMember = "ENTERPRISE_MEMBER",
        ServiceAccount = "SERVICE_ACCOUNT",
        TelephoneEndpoint = "TELEPHONE_ENDPOINT",
        TemporaryExternalParticipant = "TEMPORARY_EXTERNAL_PARTICIPANT",
        BotOrService = "BOT_OR_SERVICE",
    }
);

text_enum!(PrincipalStatus {
    Active = "ACTIVE",
    Suspended = "SUSPENDED",
    Deactivated = "DEACTIVATED",
});

impl EnvironmentKind {
    /// Human-recognisable credential prefix tag. Convenience only: the persisted
    /// environment binding of the credential is authoritative.
    pub fn credential_tag(&self) -> &'static str {
        match self {
            EnvironmentKind::Sandbox => "sbx",
            EnvironmentKind::Test => "test",
            EnvironmentKind::Production => "live",
        }
    }

    pub fn from_credential_tag(tag: &str) -> Option<Self> {
        match tag {
            "sbx" => Some(EnvironmentKind::Sandbox),
            "test" => Some(EnvironmentKind::Test),
            "live" => Some(EnvironmentKind::Production),
            _ => None,
        }
    }

    /// Only PRODUCTION environments may ever be allowed real public-network effects.
    pub fn may_allow_live_telecom(&self) -> bool {
        matches!(self, EnvironmentKind::Production)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_tags_round_trip_and_are_distinct() {
        let tags: Vec<_> = EnvironmentKind::ALL
            .iter()
            .map(|k| k.credential_tag())
            .collect();
        for kind in EnvironmentKind::ALL {
            assert_eq!(
                EnvironmentKind::from_credential_tag(kind.credential_tag()),
                Some(*kind)
            );
        }
        assert_eq!(tags.len(), 3);
        assert_ne!(tags[0], tags[1]);
        assert_ne!(tags[1], tags[2]);
        assert_eq!(EnvironmentKind::from_credential_tag("prod"), None);
    }

    #[test]
    fn only_production_may_allow_live_telecom() {
        assert!(EnvironmentKind::Production.may_allow_live_telecom());
        assert!(!EnvironmentKind::Sandbox.may_allow_live_telecom());
        assert!(!EnvironmentKind::Test.may_allow_live_telecom());
    }

    #[test]
    fn all_seven_principal_kinds_exist() {
        assert_eq!(PrincipalKind::ALL.len(), 7);
        for kind in PrincipalKind::ALL {
            assert_eq!(PrincipalKind::parse(kind.as_str()), Some(*kind));
        }
    }
}
