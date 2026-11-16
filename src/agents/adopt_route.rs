//! adopt-3e Task 10: reports go to the logical parent's mailbox (A7), decided once per notice (calls 5, 12, 19).
use super::*;

/// A7: where a member's reports go.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// The A1-resolved parent (Ended or viewless included) and its record.
    Member(String, Value),
    /// No logical parent in the tree (a root, a chain that leaves it): main's `parentPane`.
    Legacy,
}
/// A7: the logical parent from one `Tree`; several: 2 `parent` (ambiguity 5).
pub fn route(tx: &Tx<'_>, id: &str) -> Res<Route> {
    let tree = crate::agents::cascade::Tree::read(tx)?;
    match tree.parents(id)?.as_slice() {
        [] => Ok(Route::Legacy),
        [p] => {
            let r = tree.record(p).cloned().ok_or_else(evidence::unreadable)?;
            Ok(Route::Member(p.clone(), r))
        }
        _ => Err(Refusal {
            code: 2,
            check: "parent",
            message: SEVERAL.into(),
            reason: None,
            leftover: None,
            details: Default::default(),
        }),
    }
}
/// A7 for notices (AD-R7): lock-free through `stable`, in `scope` (the subject's, call 12). None: no logical parent.
pub fn notice_key(ctx: &Ctx, scope: &Scope, id: &str) -> Res<Option<String>> {
    let tx = Tx {
        ctx,
        scope: scope.clone(),
    };
    stable(ctx, scope, || {
        Ok(match route(&tx, id)? {
            Route::Member(p, r) => Some(crate::agents::mail::member_mailbox(&p, &r)),
            Route::Legacy => None,
        })
    })
}
/// Calls 12, 19: generation `n`'s notice destination, decided once and recorded (`gens/<n>/route.json`,
/// `{"mailbox": <key or null>}`) before the first delivery attempt; every retry reads it, and uses it only once its
/// publication's flushes have landed (`read_flushed`). Computed in the subject's scope (its record's session,
/// `caller`'s server key); no logical parent: main's `parentPane` key, else none. Takes no lock (AD-R7).
pub fn notice_route(
    ctx: &Ctx,
    caller: &crate::caller::Caller,
    folder: &Path,
    id: &str,
    record: &Value,
    n: u64,
) -> Res<Option<String>> {
    use crate::agents::gens;
    if let Some(v) = gens::read_flushed(folder, n, "route")? {
        return routed(&v);
    }
    let scope = Scope {
        key: Scope::of(caller).key,
        session: crate::agents::text(record.get("session")),
    };
    let key = match notice_key(ctx, &scope, id)? {
        Some(k) => Some(k),
        None => pane_key(record)?,
    };
    match gens::publish_once(folder, n, "route", &json!({ "mailbox": key }))? {
        evidence::Published::Published => Ok(key),
        evidence::Published::Exists => {
            let v = gens::read_flushed(folder, n, "route")?;
            routed(&v.ok_or_else(evidence::unreadable)?)
        }
    }
}
/// A recorded route: exactly `{"mailbox": <mailbox key or null>}` (an agent id or `pane-<n>`), else `evidence`.
fn routed(v: &Value) -> Res<Option<String>> {
    let mailbox = |k: &str| {
        crate::agents::agent_id(k)
            || k.strip_prefix("pane-")
                .is_some_and(|d| crate::agents::pane_id(&format!("%{d}")))
    };
    match v
        .as_object()
        .filter(|o| o.len() == 1)
        .and_then(|o| o.get("mailbox"))
    {
        Some(Value::Null) => Ok(None),
        Some(Value::String(k)) if mailbox(k) => Ok(Some(k.clone())),
        _ => Err(evidence::unreadable()),
    }
}
/// Main's destination: the `parentPane` mailbox, none when empty.
pub fn pane_key(record: &Value) -> Res<Option<String>> {
    let parent = crate::agents::text(record.get("parentPane"));
    if parent.is_empty() {
        Ok(None)
    } else {
        crate::agents::mail::mailbox_key(&parent, "").map(Some)
    }
}
