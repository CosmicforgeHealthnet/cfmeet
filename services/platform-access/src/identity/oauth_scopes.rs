//! The closed, versioned OAuth scope catalogue of CF Meet Apps (ADR 0064 §3; contract
//! `docs/platform/integrate-internal-contracts.md` §0).
//!
//! An app requests OAuth scopes; Platform Access maps them to the internal scopes its
//! tokens carry at authentication. The catalogue is CLOSED: an unknown name is refused
//! (`INVALID_SCOPE`), and changing it is a contract change ([`CATALOGUE_VERSION`]).
//!
//! | OAuth scope | Who | Internal scopes |
//! |---|---|---|
//! | `profile:read` | user | — |
//! | `profile:email` | user | — |
//! | `meetings:read` | user | `meetings.read` |
//! | `meetings:write` | user | `meetings.write` |
//! | `chat:read` | user | `messages.read` |
//! | `chat:write` | user | `messages.send` |
//! | `chat:read:bot` | bot | `messages.read` |
//! | `chat:write:bot` | bot | `messages.send` |
//! | `events:meetings` | install | — |
//!
//! Every internal scope reachable here is a STANDARD registry scope: `FIRST_PARTY_ONLY`
//! and `PRIVILEGED_OPERATOR` scopes are never reachable (unit-tested against the registry).
//! A USER grant may hold `user` and `install` scopes; an ORGANIZATION grant `bot` and
//! `install` scopes.

use std::collections::BTreeSet;

use serde::Serialize;

use super::scopes::names;

/// Version of the catalogue below. Any change of a name, audience or mapping bumps it.
pub const CATALOGUE_VERSION: u32 = 1;

/// Who a scope is granted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Audience {
    /// The consenting user (USER grants).
    User,
    /// The organization's bot principal (ORGANIZATION grants).
    Bot,
    /// The installation itself (either target), e.g. event delivery.
    Install,
}

/// The target of a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    User,
    Organization,
}

impl Audience {
    pub fn allowed_for(&self, target: Target) -> bool {
        matches!(
            (self, target),
            (Audience::Install, _)
                | (Audience::User, Target::User)
                | (Audience::Bot, Target::Organization)
        )
    }

    /// The `who` of the consent screen (`user` | `bot`): an install scope is shown as
    /// belonging to the grant's own principal.
    pub fn who(&self, target: Target) -> &'static str {
        match (self, target) {
            (Audience::User, _) => "user",
            (Audience::Bot, _) => "bot",
            (Audience::Install, Target::User) => "user",
            (Audience::Install, Target::Organization) => "bot",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OAuthScope {
    pub name: &'static str,
    pub description: &'static str,
    pub audience: Audience,
    /// Internal Platform Access scopes the scope maps to.
    pub internal: &'static [&'static str],
}

/// The catalogue (closed).
pub const CATALOGUE: &[OAuthScope] = &[
    OAuthScope {
        name: "profile:read",
        description: "See your name and profile picture",
        audience: Audience::User,
        internal: &[],
    },
    OAuthScope {
        name: "profile:email",
        description: "See your email address",
        audience: Audience::User,
        internal: &[],
    },
    OAuthScope {
        name: "meetings:read",
        description: "See meetings you own, host or co-host",
        audience: Audience::User,
        internal: &[names::MEETINGS_READ],
    },
    OAuthScope {
        name: "meetings:write",
        description: "Create and manage meetings you own",
        audience: Audience::User,
        internal: &[names::MEETINGS_WRITE],
    },
    OAuthScope {
        name: "chat:read",
        description: "Read messages in your managed conversations",
        audience: Audience::User,
        internal: &[names::MESSAGES_READ],
    },
    OAuthScope {
        name: "chat:write",
        description: "Send messages as you in your managed conversations",
        audience: Audience::User,
        internal: &[names::MESSAGES_SEND],
    },
    OAuthScope {
        name: "chat:read:bot",
        description: "Read messages in managed conversations the app's bot is a member of",
        audience: Audience::Bot,
        internal: &[names::MESSAGES_READ],
    },
    OAuthScope {
        name: "chat:write:bot",
        description: "Send messages as the app's bot in managed conversations it is a member of",
        audience: Audience::Bot,
        internal: &[names::MESSAGES_SEND],
    },
    OAuthScope {
        name: "events:meetings",
        description: "Receive meeting events for this installation",
        audience: Audience::Install,
        internal: &[],
    },
];

/// Most scopes one request may name.
pub const MAX_REQUESTED: usize = 32;

pub fn find(name: &str) -> Option<&'static OAuthScope> {
    CATALOGUE.iter().find(|s| s.name == name)
}

/// Parses an RFC 6749 `scope` string (space-delimited). `None`: empty, too long, or a name
/// outside the catalogue.
pub fn parse_scope_string(value: &str) -> Option<BTreeSet<&'static str>> {
    if value.len() > 1024 {
        return None;
    }
    let mut out = BTreeSet::new();
    for part in value.split(' ').filter(|p| !p.is_empty()) {
        out.insert(find(part)?.name);
        if out.len() > MAX_REQUESTED {
            return None;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Validates a stored list (client profile): every name known; `None` otherwise.
pub fn parse_list<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<BTreeSet<&'static str>> {
    values
        .into_iter()
        .map(|v| find(v).map(|s| s.name))
        .collect()
}

/// The internal scopes of a set of OAuth scopes (unknown names contribute nothing).
pub fn internal_scopes<'a>(scopes: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    scopes
        .into_iter()
        .filter_map(find)
        .flat_map(|s| s.internal.iter().map(|i| i.to_string()))
        .collect()
}

/// Whether any of `scopes` is a bot scope (an ORGANIZATION grant then gets a bot token).
pub fn has_bot_scopes<'a>(scopes: impl IntoIterator<Item = &'a str>) -> bool {
    scopes
        .into_iter()
        .filter_map(find)
        .any(|s| s.audience == Audience::Bot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::scopes::{ScopeRegistry, ScopeTrust};

    #[test]
    fn every_internal_scope_is_a_standard_registry_scope() {
        let registry = ScopeRegistry::standard();
        for scope in CATALOGUE {
            for internal in scope.internal {
                let def = registry
                    .definition(internal)
                    .unwrap_or_else(|| panic!("{internal} is not registered"));
                assert_eq!(def.trust, ScopeTrust::Standard, "{internal}");
            }
        }
    }

    #[test]
    fn names_are_unique_and_the_catalogue_is_closed() {
        let names: BTreeSet<_> = CATALOGUE.iter().map(|s| s.name).collect();
        assert_eq!(names.len(), CATALOGUE.len());
        assert_eq!(CATALOGUE.len(), 9);
        assert_eq!(CATALOGUE_VERSION, 1);
        assert!(find("admin").is_none());
        assert!(find("meetings.read").is_none());
    }

    #[test]
    fn scope_strings_parse_strictly() {
        let s = parse_scope_string("meetings:read  chat:write meetings:read").unwrap();
        assert_eq!(
            s.into_iter().collect::<Vec<_>>(),
            vec!["chat:write", "meetings:read"]
        );
        assert!(parse_scope_string("").is_none());
        assert!(parse_scope_string("   ").is_none());
        assert!(parse_scope_string("meetings:read unknown").is_none());
        assert_eq!(
            internal_scopes(["meetings:read", "chat:write", "profile:read"]),
            ["meetings.read", "messages.send"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        assert!(has_bot_scopes(["chat:read:bot"]));
        assert!(!has_bot_scopes(["chat:read", "events:meetings"]));
    }

    #[test]
    fn audiences_follow_the_target() {
        let bot = find("chat:write:bot").unwrap().audience;
        let user = find("meetings:write").unwrap().audience;
        let install = find("events:meetings").unwrap().audience;
        assert!(bot.allowed_for(Target::Organization) && !bot.allowed_for(Target::User));
        assert!(user.allowed_for(Target::User) && !user.allowed_for(Target::Organization));
        assert!(install.allowed_for(Target::User) && install.allowed_for(Target::Organization));
        assert_eq!(install.who(Target::Organization), "bot");
    }
}
