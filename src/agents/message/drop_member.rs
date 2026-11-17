//! restore-6c P2 (#103): `drop X`, a person's cascade stop of a member of the anchor session, by exact identity.
use serde_json::{Map, Value};

use crate::cli::Request;
use crate::ctx::Ctx;
use crate::error::{Res, refuse};

/// P2 (calls 15, 25; checklist 25-27): `drop X` is `stop X` (cascade-3d) for a person only, on X of the anchor
/// session. X is resolved under a short team Tx (call 15's subject), released, then stopped by its exact id through
/// `stop_with`: `by_id` matches the id only, and `stop_life`'s `current` revalidates the identity under the lock.
/// Its released shells stay kept (views-6f V5).
pub fn drop_member(ctx: &Ctx, request: &Request) -> Res<Map<String, Value>> {
    let person = super::super::up::person_as(ctx, "drop a member")?;
    let here = super::super::scope::state(&person); // 6a call 10
    let target = super::super::view::first(request);
    let subject =
        super::super::team_state::open(ctx, &here, |tx| super::super::resume::subject(tx, target))?;
    let super::super::resume::Subject::Life(id) = subject else {
        return refuse(2, "target", "no member of this team has that name or id");
    };
    super::stop_with(ctx, |ctx, caller| {
        super::super::member_target::by_id(ctx, caller, &id)
            .map(super::super::member_target::StopTarget::Member)
    })
}
