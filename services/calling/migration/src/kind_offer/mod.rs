//! The call-kind-offer history ([`crate::CallKindOfferMigrator`], table
//! `calling_call_kind_offer_seaql_migrations`): CF2, switching a voice call to video
//! while it still rings. Its own history for the same rollback-safety reason as the
//! notification-publications one: the previous image fails closed on any migration of
//! the main or gap-closure history it does not know, and never reads this table.

pub mod m20261207_000057_call_kind_offer;
