//! IVR flows (Part F §57, §65–§75): a versioned, typed, declarative routing graph.
//!
//! Calling owns the IVR business state and flow; the media plane (Telephony / Asterisk
//! for SIP callers, the application's own client for RTC callers) only plays built-in
//! prompts and reports keypad input. A flow is never a script: nodes are typed
//! (PROMPT, MENU, ROUTE_EXTENSION, ROUTE_RING_GROUP, ROUTE_QUEUE, ROUTE_VOICEMAIL,
//! ROUTE_SCHEDULE, HANGUP), prompts are a controlled built-in catalog, and every graph is
//! validated before it can be published:
//!
//! * node ids are unique, bounded (`^[a-z][a-z0-9_]{0,31}$`), at most 32 nodes;
//! * the start node exists and every node is reachable from it;
//! * every reference names an existing node, every destination exists, is ACTIVE and
//!   belongs to the caller's scope (the database guard re-checks the scope);
//! * a menu has 1–12 distinct single-key options (`0-9`, `*`, `#`), a bounded input
//!   timeout and retry count;
//! * a cycle without a MENU (no caller input) is refused as an unbounded loop; cycles
//!   through a menu are bounded at runtime by the step budget.
//!
//! Published versions are immutable; a Call keeps the version it started with even after
//! a newer one is activated.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::{
    audit::{self, AuditDetails, AuditFact, AuditOperation, AuditResource},
    check_version, clean_name,
    destinations::{self, Destination, DestinationType},
    exec,
    policy::{self, AdminCapability, ResourceStatus},
    scope_values, stmt, time, AdminActor, EnterpriseError, EnterpriseResult, EnterpriseService,
    PageRequest,
};
use crate::calls::command::CallScope;

/// Maximum nodes of one graph.
pub const MAX_IVR_NODES: usize = 32;
/// Maximum options of one menu.
pub const MAX_MENU_OPTIONS: usize = 12;
/// Maximum node transitions of one IVR session (runtime cycle bound).
pub const MAX_IVR_STEPS: i16 = 48;

text_enum!(
    /// The controlled built-in prompt catalog. The media plane maps each to its own audio
    /// (Calling never names a provider's sound file).
    BuiltinPrompt {
        Welcome = "WELCOME",
        Menu = "MENU",
        InvalidOption = "INVALID_OPTION",
        NoInput = "NO_INPUT",
        PleaseHold = "PLEASE_HOLD",
        Transferring = "TRANSFERRING",
        Goodbye = "GOODBYE",
        Closed = "CLOSED",
        VoicemailGreeting = "VOICEMAIL_GREETING",
        Beep = "BEEP",
    }
);

impl BuiltinPrompt {
    /// The prompt's nominal length (seconds): the media plane's audio for it must fit. A
    /// menu's input timer starts after the prompts it queued; a HANGUP prompt plays for
    /// this long before the platform ends the Call.
    pub const fn nominal_seconds(self) -> i64 {
        match self {
            Self::Welcome | Self::Menu | Self::NoInput | Self::Goodbye => 2,
            Self::PleaseHold | Self::Transferring | Self::Closed => 3,
            Self::InvalidOption => 4,
            Self::VoicemailGreeting => 6,
            Self::Beep => 1,
        }
    }
}

/// A prompt reference: a built-in prompt. (Customer-provided prompt artifacts are not
/// accepted by this deployment: no audio is stored in the Calling database.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptRef {
    pub builtin: BuiltinPrompt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MenuOption {
    /// One key: `0`-`9`, `*` or `#`.
    pub digit: String,
    pub next: String,
}

/// A typed IVR node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE", deny_unknown_fields)]
pub enum IvrNode {
    Prompt {
        id: String,
        prompt: PromptRef,
        next: String,
    },
    #[serde(rename_all = "camelCase")]
    Menu {
        id: String,
        prompt: PromptRef,
        input_timeout_seconds: i32,
        max_retries: i32,
        options: Vec<MenuOption>,
        on_invalid: String,
        on_timeout: String,
    },
    #[serde(rename_all = "camelCase")]
    RouteExtension { id: String, extension_id: Uuid },
    #[serde(rename_all = "camelCase")]
    RouteRingGroup { id: String, ring_group_id: Uuid },
    #[serde(rename_all = "camelCase")]
    RouteQueue { id: String, queue_id: Uuid },
    #[serde(rename_all = "camelCase")]
    RouteVoicemail { id: String, mailbox_id: Uuid },
    #[serde(rename_all = "camelCase")]
    RouteSchedule { id: String, schedule_id: Uuid },
    Hangup {
        id: String,
        #[serde(default)]
        prompt: Option<PromptRef>,
    },
}

impl IvrNode {
    pub fn id(&self) -> &str {
        match self {
            IvrNode::Prompt { id, .. }
            | IvrNode::Menu { id, .. }
            | IvrNode::RouteExtension { id, .. }
            | IvrNode::RouteRingGroup { id, .. }
            | IvrNode::RouteQueue { id, .. }
            | IvrNode::RouteVoicemail { id, .. }
            | IvrNode::RouteSchedule { id, .. }
            | IvrNode::Hangup { id, .. } => id,
        }
    }

    /// The node ids this node can continue to.
    pub fn successors(&self) -> Vec<&str> {
        match self {
            IvrNode::Prompt { next, .. } => vec![next.as_str()],
            IvrNode::Menu {
                options,
                on_invalid,
                on_timeout,
                ..
            } => {
                let mut out: Vec<&str> = options.iter().map(|o| o.next.as_str()).collect();
                out.push(on_invalid);
                out.push(on_timeout);
                out
            }
            _ => Vec::new(),
        }
    }

    /// The configured destination of a ROUTE_* node.
    pub fn destination(&self) -> Option<Destination> {
        Some(match self {
            IvrNode::RouteExtension { extension_id, .. } => {
                Destination::new(DestinationType::Extension, *extension_id)
            }
            IvrNode::RouteRingGroup { ring_group_id, .. } => {
                Destination::new(DestinationType::RingGroup, *ring_group_id)
            }
            IvrNode::RouteQueue { queue_id, .. } => {
                Destination::new(DestinationType::Queue, *queue_id)
            }
            IvrNode::RouteVoicemail { mailbox_id, .. } => {
                Destination::new(DestinationType::Voicemail, *mailbox_id)
            }
            IvrNode::RouteSchedule { schedule_id, .. } => {
                Destination::new(DestinationType::Schedule, *schedule_id)
            }
            _ => return None,
        })
    }
}

/// A complete IVR graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IvrGraph {
    pub start_node: String,
    pub nodes: Vec<IvrNode>,
}

impl IvrGraph {
    pub fn node(&self, id: &str) -> Option<&IvrNode> {
        self.nodes.iter().find(|n| n.id() == id)
    }
}

/// Destination kinds an IVR may route to (as configured kinds become available).
pub fn route_types() -> &'static [DestinationType] {
    &[
        DestinationType::Extension,
        DestinationType::RingGroup,
        DestinationType::Queue,
    ]
}

fn valid_node_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

fn valid_digit(d: &str) -> bool {
    matches!(d.as_bytes(), [b'0'..=b'9' | b'*' | b'#'])
}

/// Structural validation (no database): ids, references, bounds, reachability and
/// input-free cycles. Destinations are validated separately against the scope.
pub fn validate_structure(graph: &IvrGraph) -> EnterpriseResult<()> {
    if graph.nodes.is_empty() || graph.nodes.len() > MAX_IVR_NODES {
        return Err(EnterpriseError::Validation(
            "an IVR graph has 1 to 32 nodes.",
        ));
    }
    let mut ids = BTreeSet::new();
    for node in &graph.nodes {
        if !valid_node_id(node.id()) {
            return Err(EnterpriseError::Validation(
                "node ids are 1-32 lower-case letters, digits or underscores, starting with a letter.",
            ));
        }
        if !ids.insert(node.id()) {
            return Err(EnterpriseError::Validation("node ids must be unique."));
        }
    }
    if !ids.contains(graph.start_node.as_str()) {
        return Err(EnterpriseError::Validation(
            "startNode must name a node of the graph.",
        ));
    }
    for node in &graph.nodes {
        for next in node.successors() {
            if !ids.contains(next) {
                return Err(EnterpriseError::Validation(
                    "a node refers to an unknown node.",
                ));
            }
        }
        if let IvrNode::Menu {
            input_timeout_seconds,
            max_retries,
            options,
            ..
        } = node
        {
            if !(2..=60).contains(input_timeout_seconds) {
                return Err(EnterpriseError::Validation(
                    "inputTimeoutSeconds must be between 2 and 60.",
                ));
            }
            if !(0..=5).contains(max_retries) {
                return Err(EnterpriseError::Validation(
                    "maxRetries must be between 0 and 5.",
                ));
            }
            if options.is_empty() || options.len() > MAX_MENU_OPTIONS {
                return Err(EnterpriseError::Validation("a menu has 1 to 12 options."));
            }
            let mut digits = BTreeSet::new();
            for option in options {
                if !valid_digit(&option.digit) {
                    return Err(EnterpriseError::Validation(
                        "an option digit is one key: 0-9, * or #.",
                    ));
                }
                if !digits.insert(option.digit.as_str()) {
                    return Err(EnterpriseError::Validation(
                        "option digits of a menu must be distinct.",
                    ));
                }
            }
        }
    }
    // Every node reachable from the start.
    let mut seen = BTreeSet::new();
    let mut stack = vec![graph.start_node.as_str()];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(node) = graph.node(id) {
            stack.extend(node.successors());
        }
    }
    if seen.len() != graph.nodes.len() {
        return Err(EnterpriseError::Validation(
            "every node must be reachable from startNode.",
        ));
    }
    // No cycle without caller input: PROMPT -> PROMPT -> ... loops would never wait.
    let prompt_edges: BTreeMap<&str, &str> = graph
        .nodes
        .iter()
        .filter_map(|n| match n {
            IvrNode::Prompt { id, next, .. } => Some((id.as_str(), next.as_str())),
            _ => None,
        })
        .collect();
    for start in prompt_edges.keys() {
        let mut visited = BTreeSet::new();
        let mut current = *start;
        while let Some(next) = prompt_edges.get(current) {
            if !visited.insert(current) {
                return Err(EnterpriseError::Validation(
                    "the graph has a loop without caller input (unbounded).",
                ));
            }
            current = next;
        }
    }
    Ok(())
}

/// Full validation: structure plus every destination in `scope`.
async fn validate_graph<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    graph: &IvrGraph,
) -> EnterpriseResult<()> {
    validate_structure(graph)?;
    for node in &graph.nodes {
        if let Some(dest) = node.destination() {
            destinations::validate(db, scope, dest, route_types()).await?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct FlowRow {
    pub id: Uuid,
    pub name: String,
    pub active_version_id: Option<Uuid>,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

const FLOW_COLUMNS: &str =
    "id, name, active_version_id, status, version, created_at, updated_at, disabled_at";

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct VersionRow {
    pub id: Uuid,
    pub flow_id: Uuid,
    pub version_number: i32,
    pub graph: Value,
    pub node_count: i16,
    pub created_at: DateTime<Utc>,
}

impl VersionRow {
    pub(crate) fn graph(&self) -> EnterpriseResult<IvrGraph> {
        serde_json::from_value(self.graph.clone())
            .map_err(|_| EnterpriseError::Unavailable("a stored IVR graph is unreadable"))
    }
}

const VERSION_COLUMNS: &str = "id, flow_id, version_number, graph, node_count, created_at";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IvrFlowView {
    pub ivr_flow_id: Uuid,
    pub name: String,
    pub active_version_number: Option<i32>,
    pub latest_version_number: Option<i32>,
    pub status: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub disabled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IvrFlowPage {
    pub ivr_flows: Vec<IvrFlowView>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IvrFlowVersionView {
    pub ivr_flow_id: Uuid,
    pub version_number: i32,
    pub node_count: i16,
    pub active: bool,
    pub graph: IvrGraph,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IvrFlowVersionList {
    pub versions: Vec<IvrFlowVersionView>,
}

pub(crate) async fn flow_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<FlowRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    FlowRow::find_by_statement(stmt(
        &format!(
            "SELECT {FLOW_COLUMNS} FROM enterprise_ivr_flows \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

/// A version of a flow by id (the version a Call runs).
pub(crate) async fn version_row<C: ConnectionTrait>(
    db: &C,
    version_id: Uuid,
) -> EnterpriseResult<VersionRow> {
    VersionRow::find_by_statement(stmt(
        &format!("SELECT {VERSION_COLUMNS} FROM enterprise_ivr_flow_versions WHERE id = $1"),
        vec![version_id.into()],
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

async fn versions<C: ConnectionTrait>(db: &C, flow_id: Uuid) -> EnterpriseResult<Vec<VersionRow>> {
    Ok(VersionRow::find_by_statement(stmt(
        &format!(
            "SELECT {VERSION_COLUMNS} FROM enterprise_ivr_flow_versions WHERE flow_id = $1 \
             ORDER BY version_number DESC LIMIT 100"
        ),
        vec![flow_id.into()],
    ))
    .all(db)
    .await?)
}

async fn flow_view<C: ConnectionTrait>(db: &C, row: FlowRow) -> EnterpriseResult<IvrFlowView> {
    let numbers = db
        .query_one(stmt(
            "SELECT (SELECT version_number FROM enterprise_ivr_flow_versions WHERE id = $2), \
                    (SELECT max(version_number) FROM enterprise_ivr_flow_versions WHERE flow_id = $1)",
            vec![row.id.into(), row.active_version_id.into()],
        ))
        .await?;
    let (active, latest) = match numbers {
        Some(r) => (
            r.try_get_by_index::<Option<i32>>(0)?,
            r.try_get_by_index::<Option<i32>>(1)?,
        ),
        None => (None, None),
    };
    Ok(IvrFlowView {
        ivr_flow_id: row.id,
        name: row.name,
        active_version_number: active,
        latest_version_number: latest,
        status: row.status,
        version: row.version,
        created_at: time(row.created_at),
        updated_at: time(row.updated_at),
        disabled_at: row.disabled_at.map(time),
    })
}

fn version_view(row: &VersionRow, active: Option<Uuid>) -> EnterpriseResult<IvrFlowVersionView> {
    Ok(IvrFlowVersionView {
        ivr_flow_id: row.flow_id,
        version_number: row.version_number,
        node_count: row.node_count,
        active: active == Some(row.id),
        graph: row.graph()?,
        created_at: time(row.created_at),
    })
}

impl EnterpriseService {
    pub async fn create_ivr_flow(
        &self,
        actor: &AdminActor,
        name: String,
    ) -> EnterpriseResult<IvrFlowView> {
        let name = clean_name(&name, 100, "name must be 1-100 characters.")?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        let id = Uuid::new_v4();
        let now = self.now();
        let mut values = vec![id.into()];
        values.extend(scope_values(actor.scope));
        values.extend([name.into(), now.into()]);
        exec(
            &txn,
            "INSERT INTO enterprise_ivr_flows (id, tenant_id, application_id, environment_id, name, \
             status, version, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, 'ACTIVE', 1, $6, $6)",
            values,
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::IvrFlow,
                resource_id: id,
                operation: AuditOperation::Create,
                old_version: None,
                new_version: Some(1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let out = flow_view(&txn, flow_row(&txn, actor.scope, id, false).await?).await?;
        txn.commit().await?;
        Ok(out)
    }

    pub async fn get_ivr_flow(&self, scope: CallScope, id: Uuid) -> EnterpriseResult<IvrFlowView> {
        flow_view(self.db(), flow_row(self.db(), scope, id, false).await?).await
    }

    pub async fn list_ivr_flows(
        &self,
        scope: CallScope,
        status: Option<ResourceStatus>,
        page: &PageRequest,
    ) -> EnterpriseResult<IvrFlowPage> {
        let limit = page.limit()?;
        let mut values = scope_values(scope);
        let mut sql = format!(
            "SELECT {FLOW_COLUMNS} FROM enterprise_ivr_flows \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3"
        );
        if let Some(status) = status {
            values.push(status.as_str().into());
            sql.push_str(&format!(" AND status = ${}", values.len()));
        }
        if let Some((at, id)) = page.after()? {
            values.push(at.into());
            values.push(id.into());
            sql.push_str(&format!(
                " AND (created_at, id) > (${}, ${})",
                values.len() - 1,
                values.len()
            ));
        }
        sql.push_str(&format!(" ORDER BY created_at, id LIMIT {}", limit + 1));
        let mut rows = FlowRow::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|r| super::cursor::encode(r.created_at, r.id))
        } else {
            None
        };
        let mut ivr_flows = Vec::with_capacity(rows.len());
        for row in rows {
            ivr_flows.push(flow_view(self.db(), row).await?);
        }
        Ok(IvrFlowPage {
            ivr_flows,
            next_cursor,
        })
    }

    pub async fn rename_ivr_flow(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        name: String,
    ) -> EnterpriseResult<IvrFlowView> {
        let name = clean_name(&name, 100, "name must be 1-100 characters.")?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        let row = flow_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled IVR flow cannot be changed",
            ));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_ivr_flows SET name = $2, version = version + 1, \
             updated_at = GREATEST(updated_at, $3) WHERE id = $1",
            vec![id.into(), name.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::IvrFlow,
                resource_id: id,
                operation: AuditOperation::Update,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let out = flow_view(&txn, flow_row(&txn, actor.scope, id, false).await?).await?;
        txn.commit().await?;
        Ok(out)
    }

    /// Publishes a new immutable version of a flow (validated; not yet active).
    pub async fn publish_ivr_version(
        &self,
        actor: &AdminActor,
        id: Uuid,
        graph: IvrGraph,
    ) -> EnterpriseResult<IvrFlowVersionView> {
        validate_structure(&graph)?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        let row = flow_row(&txn, actor.scope, id, true).await?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled IVR flow cannot be changed",
            ));
        }
        validate_graph(&txn, actor.scope, &graph).await?;
        for node in &graph.nodes {
            if node.destination().map(|d| d.kind) == Some(DestinationType::Ivr)
                && node.destination().map(|d| d.id) == Some(id)
            {
                return Err(EnterpriseError::Validation(
                    "an IVR flow cannot route to itself.",
                ));
            }
        }
        let number = txn
            .query_one(stmt(
                "SELECT (COALESCE(max(version_number), 0) + 1)::int FROM enterprise_ivr_flow_versions \
                 WHERE flow_id = $1",
                vec![id.into()],
            ))
            .await?
            .map(|r| r.try_get_by_index::<i32>(0))
            .transpose()?
            .unwrap_or(1);
        let version_id = Uuid::new_v4();
        let now = self.now();
        let json = serde_json::to_value(&graph)
            .map_err(|_| EnterpriseError::Validation("the graph cannot be stored."))?;
        let mut values = vec![version_id.into()];
        values.extend(scope_values(actor.scope));
        values.extend([
            id.into(),
            number.into(),
            json.into(),
            (graph.nodes.len() as i16).into(),
            graph.start_node.clone().into(),
            now.into(),
        ]);
        exec(
            &txn,
            "INSERT INTO enterprise_ivr_flow_versions (id, tenant_id, application_id, environment_id, \
             flow_id, version_number, graph, node_count, start_node, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            values,
        )
        .await?;
        for node in &graph.nodes {
            if let Some(dest) = node.destination() {
                let mut values = scope_values(actor.scope);
                values.extend([
                    version_id.into(),
                    node.id().to_string().into(),
                    dest.kind.as_str().into(),
                    dest.id.into(),
                ]);
                exec(
                    &txn,
                    "INSERT INTO enterprise_ivr_flow_destinations (tenant_id, application_id, \
                     environment_id, flow_version_id, node_id, destination_type, destination_id) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7)",
                    values,
                )
                .await?;
            }
        }
        exec(
            &txn,
            "UPDATE enterprise_ivr_flows SET version = version + 1, \
             updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::IvrFlow,
                resource_id: id,
                operation: AuditOperation::PublishVersion,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new()
                    .with("version_number", number as usize)
                    .with("node_count", graph.nodes.len()),
            },
            now,
        )
        .await?;
        let version = version_row(&txn, version_id).await?;
        let out = version_view(&version, row.active_version_id)?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_config_changes_total",
            &[("resource", "IVR_FLOW"), ("operation", "PUBLISH_VERSION")],
        );
        Ok(out)
    }

    /// Makes a published version the one NEW Calls run (Calls already in the IVR keep
    /// theirs). Re-validates its destinations: a version whose targets were disabled
    /// since publishing cannot be activated.
    pub async fn activate_ivr_version(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        version_number: i32,
    ) -> EnterpriseResult<IvrFlowView> {
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        let row = flow_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled IVR flow cannot be changed",
            ));
        }
        let version = VersionRow::find_by_statement(stmt(
            &format!(
                "SELECT {VERSION_COLUMNS} FROM enterprise_ivr_flow_versions \
                 WHERE flow_id = $1 AND version_number = $2"
            ),
            vec![id.into(), version_number.into()],
        ))
        .one(&txn)
        .await?
        .ok_or(EnterpriseError::NotFound)?;
        validate_graph(&txn, actor.scope, &version.graph()?).await?;
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_ivr_flows SET active_version_id = $2, version = version + 1, \
             updated_at = GREATEST(updated_at, $3) WHERE id = $1",
            vec![id.into(), version.id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::IvrFlow,
                resource_id: id,
                operation: AuditOperation::Activate,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new().with("version_number", version_number as usize),
            },
            now,
        )
        .await?;
        let out = flow_view(&txn, flow_row(&txn, actor.scope, id, false).await?).await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_config_changes_total",
            &[("resource", "IVR_FLOW"), ("operation", "ACTIVATE")],
        );
        Ok(out)
    }

    pub async fn list_ivr_versions(
        &self,
        scope: CallScope,
        id: Uuid,
    ) -> EnterpriseResult<IvrFlowVersionList> {
        let row = flow_row(self.db(), scope, id, false).await?;
        let mut out = Vec::new();
        for v in versions(self.db(), id).await? {
            out.push(version_view(&v, row.active_version_id)?);
        }
        Ok(IvrFlowVersionList { versions: out })
    }

    pub async fn get_ivr_version(
        &self,
        scope: CallScope,
        id: Uuid,
        number: i32,
    ) -> EnterpriseResult<IvrFlowVersionView> {
        let row = flow_row(self.db(), scope, id, false).await?;
        let version = VersionRow::find_by_statement(stmt(
            &format!(
                "SELECT {VERSION_COLUMNS} FROM enterprise_ivr_flow_versions \
                 WHERE flow_id = $1 AND version_number = $2"
            ),
            vec![id.into(), number.into()],
        ))
        .one(self.db())
        .await?
        .ok_or(EnterpriseError::NotFound)?;
        version_view(&version, row.active_version_id)
    }

    /// Disables a flow: new Calls routed to it fail safe; Calls already inside finish
    /// their version. Never blocked.
    pub async fn disable_ivr_flow(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
    ) -> EnterpriseResult<IvrFlowView> {
        let txn = self.begin().await?;
        let row = flow_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status == "DISABLED" {
            return Err(EnterpriseError::Conflict(
                "the IVR flow is already disabled",
            ));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_ivr_flows SET status = 'DISABLED', disabled_at = $2, \
             version = version + 1, updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::IvrFlow,
                resource_id: id,
                operation: AuditOperation::Disable,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let out = flow_view(&txn, flow_row(&txn, actor.scope, id, false).await?).await?;
        txn.commit().await?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(value: serde_json::Value) -> Result<IvrGraph, serde_json::Error> {
        serde_json::from_value(value)
    }

    fn menu(options: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "id": "main", "kind": "MENU", "prompt": { "builtin": "MENU" },
            "inputTimeoutSeconds": 5, "maxRetries": 2, "options": options,
            "onInvalid": "bye", "onTimeout": "bye" })
    }

    fn bye() -> serde_json::Value {
        serde_json::json!({ "id": "bye", "kind": "HANGUP", "prompt": { "builtin": "GOODBYE" } })
    }

    #[test]
    fn a_valid_graph_passes() {
        let g = graph(serde_json::json!({ "startNode": "hello", "nodes": [
            { "id": "hello", "kind": "PROMPT", "prompt": { "builtin": "WELCOME" }, "next": "main" },
            menu(serde_json::json!([{ "digit": "1", "next": "bye" }, { "digit": "#", "next": "main" }])),
            bye(),
        ]}))
        .unwrap();
        validate_structure(&g).unwrap();
    }

    #[test]
    fn scripts_unknown_kinds_and_fields_never_parse() {
        for bad in [
            serde_json::json!({ "startNode": "a", "nodes": [{ "id": "a", "kind": "SCRIPT", "code": "rm -rf /" }] }),
            serde_json::json!({ "startNode": "a", "nodes": [{ "id": "a", "kind": "HANGUP", "dialplan": "Dial(PJSIP/x)" }] }),
            serde_json::json!({ "startNode": "a", "nodes": [{ "id": "a", "kind": "PROMPT",
                "prompt": { "builtin": "WELCOME", "url": "http://x/a.wav" }, "next": "a" }] }),
            serde_json::json!({ "startNode": "a", "nodes": [{ "id": "a", "kind": "PROMPT",
                "prompt": { "builtin": "tt-monkeys" }, "next": "a" }] }),
        ] {
            assert!(graph(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn invalid_graphs_are_refused() {
        let cases = [
            // unknown start
            serde_json::json!({ "startNode": "nope", "nodes": [bye()] }),
            // unknown reference
            serde_json::json!({ "startNode": "main", "nodes": [menu(serde_json::json!([{ "digit": "1", "next": "ghost" }])), bye()] }),
            // invalid digit / multi-digit / duplicate digit / empty options
            serde_json::json!({ "startNode": "main", "nodes": [menu(serde_json::json!([{ "digit": "12", "next": "bye" }])), bye()] }),
            serde_json::json!({ "startNode": "main", "nodes": [menu(serde_json::json!([{ "digit": "a", "next": "bye" }])), bye()] }),
            serde_json::json!({ "startNode": "main", "nodes": [menu(serde_json::json!([{ "digit": "1", "next": "bye" }, { "digit": "1", "next": "main" }])), bye()] }),
            serde_json::json!({ "startNode": "main", "nodes": [menu(serde_json::json!([])), bye()] }),
            // unreachable node
            serde_json::json!({ "startNode": "bye", "nodes": [bye(), menu(serde_json::json!([{ "digit": "1", "next": "bye" }]))] }),
            // input-free loop
            serde_json::json!({ "startNode": "a", "nodes": [
                { "id": "a", "kind": "PROMPT", "prompt": { "builtin": "WELCOME" }, "next": "b" },
                { "id": "b", "kind": "PROMPT", "prompt": { "builtin": "WELCOME" }, "next": "a" }] }),
            serde_json::json!({ "startNode": "a", "nodes": [
                { "id": "a", "kind": "PROMPT", "prompt": { "builtin": "WELCOME" }, "next": "a" }] }),
            // bad id, duplicate id
            serde_json::json!({ "startNode": "Bad Id", "nodes": [{ "id": "Bad Id", "kind": "HANGUP" }] }),
            serde_json::json!({ "startNode": "bye", "nodes": [bye(), bye()] }),
        ];
        for case in cases {
            let g = graph(case.clone()).unwrap();
            assert!(validate_structure(&g).is_err(), "{case}");
        }
        // Size bound.
        let mut nodes = vec![serde_json::json!({ "id": "n0", "kind": "PROMPT",
            "prompt": { "builtin": "WELCOME" }, "next": "n1" })];
        for i in 1..33 {
            nodes.push(serde_json::json!({ "id": format!("n{i}"), "kind": "PROMPT",
                "prompt": { "builtin": "WELCOME" }, "next": format!("n{}", i + 1) }));
        }
        nodes.push(serde_json::json!({ "id": "n33", "kind": "HANGUP" }));
        let g = graph(serde_json::json!({ "startNode": "n0", "nodes": nodes })).unwrap();
        assert!(validate_structure(&g).is_err());
        // Bounds of a menu.
        let mut m = menu(serde_json::json!([{ "digit": "1", "next": "bye" }]));
        m["maxRetries"] = serde_json::json!(50);
        let g = graph(serde_json::json!({ "startNode": "main", "nodes": [m, bye()] })).unwrap();
        assert!(validate_structure(&g).is_err());
    }
}
