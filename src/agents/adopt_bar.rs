//! adopt-3e Task 9: lock-free readers (`stable`, A6), the finder (`candidates`, A8), the sweep and the bar's one try
//! (`tick`, call 4), the entry for a team-lock holder (`adopt_locked`, prelife-1c call 2).
use super::*;

/// A6: a lock-free reader saw the tree change on every try.
pub const MOVING: &str = "the team tree is changing; retry";

/// The committed seq and the names in `.adopt`: what a lock-free reader compares (names only: call 14).
fn mark(ctx: &Ctx, scope: &Scope) -> Res<(u64, Listing)> {
    Ok((committed(ctx, scope)?, listing(ctx, scope)?))
}
/// A6 (ambiguity 11): mark, `read`, mark. Equal and nothing journalled: the answer. Else retry; four reads: MOVING.
/// A journal that vanished between the listing and its leaf check is gone (`kept`), so a moving tree is a retry.
pub fn stable<T>(ctx: &Ctx, scope: &Scope, mut read: impl FnMut() -> Res<T>) -> Res<T> {
    for _ in 0..4 {
        let before = mark(ctx, scope)?;
        let got = read()?;
        if before.1 == Listing::default() && mark(ctx, scope)? == before {
            return Ok(got);
        }
    }
    Err(refusal("topology", MOVING))
}
/// A8's finder (ambiguity 12, call 11), lock-free over one `Tree`: every current, non-root logical parent of some
/// member whose active generation is Ended. `adopt_from` revalidates under the lock.
pub fn candidates(ctx: &Ctx, scope: &Scope) -> Res<Vec<String>> {
    let tree = crate::agents::cascade::Tree::read(&Tx {
        ctx,
        scope: scope.clone(),
    })?;
    let mut parents = BTreeSet::new();
    for id in tree.ids() {
        parents.extend(tree.parents(id)?);
    }
    let mut out = Vec::new();
    for p in parents {
        let Some(r) = tree.record(&p) else { continue };
        // `Tree::parents` resolves C0, so a superseded member is never a logical parent (mutation 9.9 survived).
        if crate::agents::tree::is_leader(r) {
            continue;
        }
        let folder = scope.agents(&ctx.state_root).join(&p);
        let ended = |n| {
            matches!(
                crate::agents::gens::read(&folder, n, "ended").known(),
                Ok(Some(_))
            )
        };
        if crate::agents::gens::active(r).is_some_and(ended) {
            out.push(p);
        }
    }
    Ok(out)
}
/// What one sweep did: any child moved; each blocked source with its reason (call 9).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Swept {
    pub moved: bool,
    pub blocked: Vec<(String, &'static str)>,
}
/// A8: every candidate, in id order.
pub fn sweep(tx: &Tx<'_>) -> Res<Swept> {
    let mut out = Swept::default();
    for id in candidates(tx.ctx, &tx.scope)? {
        match adopt_from(tx, &id)? {
            Adopted::Moved(_) => out.moved = true,
            Adopted::Blocked(why) => out.blocked.push((id, why)),
            Adopted::Nothing => {}
        }
    }
    Ok(out)
}
/// A8, prelife-1c call 2: the entry for a team-lock holder. The caller must be of the Tx's team.
pub fn adopt_locked(tx: &Tx<'_>, caller: &crate::caller::Caller) -> Res<bool> {
    if Scope::of(caller) != tx.scope {
        return Err(Refusal::internal("the caller is not of this team"));
    }
    Ok(sweep(tx)?.moved)
}
/// A8, call 4, the bar: a lock-free look, then one try of the team lock. Under that one Tx cascade-3d's `finish`
/// runs first (C8), then the sweep. Idle or busy: None, nothing changed.
pub fn tick(ctx: &Ctx, caller: &crate::caller::Caller) -> Res<Option<Swept>> {
    let scope = Scope::of(caller);
    let idle = !crate::agents::cascade::waiting(ctx, &scope)
        && listing(ctx, &scope)? == Listing::default()
        && candidates(ctx, &scope)?.is_empty();
    if idle {
        return Ok(None);
    }
    crate::agents::try_lock(ctx, &scope.team_lock(), || {
        crate::agents::team_state::held(ctx, &scope, |tx| {
            crate::agents::cascade::finish(tx, caller)?;
            sweep(tx)
        })
    })
}
