use sea_orm::entity::prelude::*;

use crate::{
    calls::kinds::{RouteClass, RouteState},
    routing::RoutingClass,
};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_routes")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub call_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub route_class: RouteClass,
    pub state: RouteState,
    pub policy_version: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub decision_snapshot: Json,
    pub selected_at: DateTimeUtc,
    pub ended_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub routing_class: Option<RoutingClass>,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub route_plan: Option<Json>,
    pub plan_fingerprint: Option<String>,
    pub selected_route_ref: Option<String>,
    pub max_attempts: Option<i16>,
    pub plan_expires_at: Option<DateTimeUtc>,
    pub route_version: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
