//! Offline examples use the same DTOs as admission, journaling and live replies.
//! No compositor connection or lifecycle effect is performed.
use super::{
    CloseRecord, Command, CommandKind, Context, Create, Lease, Placement, Purpose, Reservation,
    Target,
    replies::{CommandReply, ContextReply, LeaseReply, RecordedReply},
    wire,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

fn case<P: Serialize + DeserializeOwned>(
    method: &str,
    params: P,
    success: impl Serialize,
    limited: impl Serialize,
    invalid: Option<Value>,
) -> Value {
    let params = serde_json::to_value(params).unwrap();
    crate::protocol::decode_request::<P>(params.clone(), method).expect("valid workspace fixture");
    let rejected = invalid.as_ref().map(|invalid| {
        crate::protocol::decode_request::<P>(invalid.clone(), method)
            .err()
            .expect("invalid workspace fixture must be rejected")
    });
    json!({
        "params": params,
        "success": crate::protocol::success(json!({"workspace_operation": success})),
        "limited": crate::protocol::success(json!({"workspace_operation": limited})),
        "failed": crate::protocol::error("workspace-operation-failed", "stale workspace context"),
        "invalid_params": invalid,
        "rejected": rejected,
    })
}

pub(crate) fn control_fixtures() -> Map<String, Value> {
    let context = Context {
        session: "fixture-session".into(),
        monitor: "DP-1".into(),
        workspace_id: 1,
    };
    let create = Create {
        request_id: "fixture-create".into(),
        context: context.clone(),
        purpose: Purpose::Launch,
        workspace_id: None,
        origin: "Editor".into(),
        icon: "editor".into(),
    };
    let lease = Lease {
        session: context.session.clone(),
        request_id: create.request_id.clone(),
        workspace_id: 6,
        generation: "fixture-generation".into(),
    };
    let reserved = Reservation {
        request: create.clone(),
        lease: lease.clone(),
        writer: "fixture-writer".into(),
        released: false,
        placed: false,
        placement: None,
        status: "reserved".into(),
    };
    let mut unknown = reserved.clone();
    unknown.released = true;
    unknown.status = "unknown".into();
    let mut landing = reserved.clone();
    landing.request.purpose = Purpose::LandingPad;
    landing.request.origin = "Workspace".into();
    landing.request.icon = "workspace".into();
    landing.released = true;
    landing.status = "focused".into();
    let mut unknown_landing = landing.clone();
    unknown_landing.released = true;
    unknown_landing.status = "unknown".into();
    let close = wire::Close {
        request_id: "fixture-close".into(),
        context: context.clone(),
    };
    let window = wire::WindowIdentity {
        address: "0x123".into(),
        pid: 42,
        workspace_id: 1,
    };
    let closed = CloseRecord {
        request: close.clone(),
        targets: vec![Target {
            window: window.clone(),
            token: "fixture-window".into(),
            attempted: true,
        }],
        status: "closed".into(),
        error: None,
    };
    let mut still_open = closed.clone();
    still_open.status = "still-open".into();
    let placed = LeaseReply {
        status: "placed".into(),
        lease: lease.clone(),
    };
    let released = LeaseReply {
        status: "released".into(),
        lease: lease.clone(),
    };
    let captured = ContextReply {
        version: wire::VERSION,
        context: context.clone(),
        request_id: "fixture-context".into(),
    };
    Map::from_iter([
        (
            "workspace.context".into(),
            case("workspace.context", json!({}), &captured, &captured, None),
        ),
        (
            "workspace.command".into(),
            case(
                "workspace.command",
                Command {
                    command: CommandKind::Create,
                    workspace_id: None,
                    monitor: Some(context.monitor.clone()),
                    request_id: Some("fixture-create".into()),
                },
                CommandReply::Recorded(RecordedReply::Reservation(&landing)),
                CommandReply::Recorded(RecordedReply::Reservation(&unknown_landing)),
                Some(json!({})),
            ),
        ),
        (
            "workspace.create".into(),
            case(
                "workspace.create",
                create,
                &reserved,
                &unknown,
                Some(json!({})),
            ),
        ),
        (
            "workspace.close".into(),
            case(
                "workspace.close",
                close,
                &closed,
                &still_open,
                Some(json!({})),
            ),
        ),
        // Repeated placement/release returns the same receipt, not another effect.
        (
            "workspace.placement.apply".into(),
            case(
                "workspace.placement.apply",
                Placement {
                    lease: lease.clone(),
                    window,
                    follow: true,
                },
                &placed,
                &placed,
                Some(json!({})),
            ),
        ),
        (
            "workspace.lease.release".into(),
            case(
                "workspace.lease.release",
                lease,
                &released,
                &released,
                Some(json!({})),
            ),
        ),
        (
            "workspace.operation.get".into(),
            case(
                "workspace.operation.get",
                wire::Lookup {
                    session: context.session,
                    request_id: "fixture-create".into(),
                },
                RecordedReply::Reservation(&reserved),
                RecordedReply::Reservation(&unknown),
                Some(json!({})),
            ),
        ),
    ])
}
