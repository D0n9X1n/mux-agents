use super::core_identity::Identity;
use super::{core_facts, registry};
use crate::hc;
use crate::hc::Core;
use crate::{
    caller::Caller,
    ctx::Ctx,
    error::{Refusal, Res},
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub struct Stored {
    pub record: Value,
    pub committed: bool,
    pub noticed: bool,
    /// A lifecycle member's notice, read once here (R1); the board shows it (Task 5).
    pub notice: Option<Value>,
    /// R3: the active generation's own done (`Status.active_done`); None pre-lifecycle (outcome-2 O6).
    pub active_done: Option<bool>,
}

#[derive(Debug)]
pub struct Local {
    pub identity: Identity,
    pub stored: Stored,
    /// R4: the identity is a pending generation's.
    pub held: bool,
    /// R3: Some(finished) for a lifecycle member; None for a pre-lifecycle one.
    pub finished: Option<bool>,
}

pub fn team(caller: &Caller) -> String {
    format!("{}:{}", caller.server_key, caller.session)
}

pub fn locals(ctx: &Ctx, caller: &Caller) -> Res<Vec<Local>> {
    Ok(scan(ctx, caller)?.local)
}

/// The (id, generation) pairs a lifecycle record names that are done by recorded evidence (R3).
type Freed = Vec<(String, String)>;

/// One scan: the local members, the done (id, generation) pairs (R3), each unresolved orphan (b-calls 11) as
/// (id, generation, name), and the pre-lifecycle slots (prelife-1c G10a).
struct Scan {
    local: Vec<Local>,
    freed: Freed,
    orphans: Vec<(String, String, String)>,
    /// Held pre-lifecycle folders with no usable identity, one slot each.
    held: Vec<String>,
    /// prelife-1c calls 13, 23: the `hcAddress` of every held or freed pre-lifecycle folder; a listed terminal named
    /// here is that folder's, never another root's reservation.
    pre_addresses: Vec<String>,
    /// Pre-lifecycle identities whose folder notice proves an end (prelife-1c ambiguity 6).
    pre_ended: BTreeSet<String>,
    /// prelife-1c ambiguity 14: every same-session pre-lifecycle folder, id to name (else its id).
    pre: BTreeMap<String, String>,
}

impl Scan {
    /// `freed` and each orphan's (id, generation): `core_facts::take` never reserves their listed terminals, so an
    /// orphan's own terminal is not counted twice.
    fn skip(&self) -> Freed {
        let orphans = self.orphans.iter().map(|(i, n, _)| (i.clone(), n.clone()));
        self.freed.iter().cloned().chain(orphans).collect()
    }
    /// b-calls 11: each unresolved orphan holds the member's name and one slot, whatever hc shows.
    fn reserve(&self, facts: &mut core_facts::Snapshot) {
        let held = self
            .orphans
            .iter()
            .map(|(i, n, name)| (format!("{i}/{n}"), name.clone()));
        facts.reserved.extend(held);
    }
}

/// R1, R5: every member folder classified once; with the (id, generation) pairs a lifecycle record names that are
/// done by recorded evidence (R3), whose listed hc terminals `core_facts::take` never reserves.
fn scan(ctx: &Ctx, caller: &Caller) -> Res<Scan> {
    use super::gens::Member;
    use super::prelife::Slot;
    let (mut local, mut freed, mut orphans) = (Vec::new(), Vec::new(), Vec::new());
    let (mut held, mut pre_addresses, mut pre_ended) = (Vec::new(), Vec::new(), BTreeSet::new());
    let mut pre = BTreeMap::new();
    // R5: link entries are kept, so member_of refuses them; a regular file at an id is no member.
    for (id, folder, member) in
        super::gens::members(&super::server_dir(ctx, caller).join("agents"))?
    {
        let record = match &member {
            Member::Absent => continue,
            Member::Unreadable(e) => return Err(e.clone()),
            Member::Pre(r) => r,
            Member::Life(g) => g.record(),
        };
        // prelife-1c ambiguity 5: a record naming another session is skipped; one naming none (or not an object)
        // is the caller's (fail closed).
        if record
            .get("session")
            .and_then(Value::as_str)
            .is_some_and(|s| s != caller.session)
        {
            continue;
        }
        let one = match &member {
            Member::Life(g) => {
                let s = g.status()?; // R1: the one status read; life_local takes it
                for (n, done) in [(g.pending(), s.pending_done), (g.active(), s.active_done)] {
                    if let (Some(n), Some(true)) = (n, done) {
                        freed.push((id.clone(), n.to_string()));
                    }
                }
                // b-calls 11: an Unreadable orphan refuses the scan, as `status()?` does.
                let name = record
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                // Call 61: a done orphan is skipped (its listed terminal holds nothing), never reserved.
                for (n, done) in g.unnamed()? {
                    if done {
                        freed.push((id.clone(), n.to_string()));
                    } else {
                        orphans.push((id.clone(), n.to_string(), name.to_owned()));
                    }
                }
                // r6b call 12: a held pending n+1 stands for the member (one slot); its listed active n is skipped.
                if let (true, Some(n)) = (s.held, g.active()) {
                    freed.push((id.clone(), n.to_string()));
                }
                life_local(g, &s, &id, &team(caller))?
            }
            _ => {
                // Ambiguity 14: the `limit` refusal names each held one by this (its name, else its id).
                let name = record.get("name").and_then(Value::as_str).unwrap_or(&id);
                pre.insert(id.clone(), name.to_string());
                match super::prelife::slot(&folder, record, &id, caller) {
                    Slot::Local(l) => {
                        if super::prelife::ended_by_notice(&folder) {
                            pre_ended.insert(id.clone());
                        }
                        Some(*l)
                    }
                    s => {
                        if let Some(a) = record.get("hcAddress").and_then(Value::as_str) {
                            pre_addresses.push(a.to_string());
                        }
                        if matches!(s, Slot::Held) {
                            held.push(id.clone());
                        }
                        None
                    }
                }
            }
        };
        local.extend(one);
    }
    Ok(Scan {
        local,
        freed,
        orphans,
        held,
        pre_addresses,
        pre_ended,
        pre,
    })
}

/// R1-R4: one `status`, the subject generation's identity, commit and notice (each read once), and its
/// presentation; none for a held identity. A finished member is kept with `finished: Some(true)` so `settle` reads
/// it Ended; a member that never had a subject is no member.
fn life_local(
    g: &super::gens::Gen,
    s: &super::gens::Status,
    id: &str,
    team: &str,
) -> Res<Option<Local>> {
    let Some(n) = s.subject else {
        return Ok(None);
    };
    let (identity, record) = subject_of(g, s, id, team)?;
    let notice = g.read(n, "notice").known()?;
    Ok(Some(Local {
        identity,
        stored: Stored {
            record,
            committed: s.committed,
            noticed: notice.is_some(),
            notice,
            active_done: s.active_done,
        },
        held: s.held,
        finished: Some(s.finished),
    }))
}

/// R4: the identity and record of the generation `s.subject` names, built once for membership and for
/// `member_target::current`, so selection and revalidation agree. A held pending generation: `Identity::held`, the
/// record without presentation fields (the active generation's view is not its). Otherwise `Identity::read` and the
/// presented record. Call only when `s.subject` is Some.
pub(crate) fn subject_of(
    g: &super::gens::Gen,
    s: &super::gens::Status,
    id: &str,
    team: &str,
) -> Res<(Identity, Value)> {
    if s.held {
        let identity = Identity::held(g.record(), id, team)
            .ok_or_else(|| Refusal::internal("invalid held generation"))?;
        return Ok((identity, super::team_state::presented(g, s)?));
    }
    let identity = Identity::read(g.record(), id, team)
        .ok_or_else(|| Refusal::internal("invalid local hc identity"))?;
    Ok((identity, super::team_state::presented(g, s)?))
}

/// R3 (ambiguity 5): a lifecycle observation is Ended exactly when every generation its record names is done;
/// hc's Ended alone becomes Unknown, and a recorded end with hc forgetting the terminal is Ended.
fn settle(facts: &mut core_facts::Snapshot, local: &[Local]) {
    use super::lifecycle::Life;
    for o in &mut facts.local {
        match local
            .iter()
            .find(|l| l.identity.id == o.identity.id)
            .and_then(|l| l.finished)
        {
            Some(true) => o.life = Life::Ended,
            Some(false) if o.life == Life::Ended => o.life = Life::Unknown,
            _ => {}
        }
    }
}

/// The ids a committed member names in `resumedFrom`: each is superseded by its resume, so its name means the
/// successor and it is never the one a name, list, board, send or focus picks. Supersession follows the
/// `resumedFrom` chain (amendment 2 to resume v11, recorded on #103): a superseded member's own `resumedFrom` is
/// superseded too, so X -> Y -> Z supersedes X even when Y (a successor whose claim was not abandoned but which has
/// no commit, because it may have run) never committed.
pub fn superseded(records: &[(&str, &Value, bool)]) -> std::collections::BTreeSet<String> {
    let mut superseded = std::collections::BTreeSet::new();
    loop {
        let before = superseded.len();
        for (id, record, committed) in records {
            if !*committed && !superseded.contains(*id) {
                continue;
            }
            if let Some(from) = record
                .get("resumedFrom")
                .and_then(Value::as_str)
                .filter(|from| super::agent_id(from))
            {
                superseded.insert(from.to_owned());
            }
        }
        if superseded.len() == before {
            return superseded;
        }
    }
}

/// `superseded` over every local member.
pub fn superseded_locals(local: &[Local]) -> std::collections::BTreeSet<String> {
    let rows: Vec<_> = local
        .iter()
        .map(|l| (l.identity.id.as_str(), &l.stored.record, l.stored.committed))
        .collect();
    superseded(&rows)
}

#[derive(Debug)]
pub struct Snapshot {
    pub facts: core_facts::Snapshot,
    pub records: BTreeMap<String, Stored>,
    pub views: BTreeMap<String, registry::Pane>,
    pub view_order: Vec<String>,
    pub core_error: Option<Refusal>,
    pub view_error: Option<Refusal>,
    /// prelife-1c G10a: held pre-lifecycle folders (no usable identity), one team slot each.
    pub held: Vec<String>,
    /// prelife-1c ambiguity 6: pre-lifecycle members whose folder notice proves an end; listed, holding no slot.
    pub pre_ended: BTreeSet<String>,
    /// prelife-1c ambiguity 14: every same-session pre-lifecycle folder, id to name (else its id); the `limit`
    /// refusal names the held ones from it.
    pub pre: BTreeMap<String, String>,
}

impl Snapshot {
    /// Whether an observed member holds a team slot: not Ended, and not a pre-lifecycle member its notice ended.
    pub fn holds(&self, o: &core_facts::Observation) -> bool {
        o.life != super::lifecycle::Life::Ended && !self.pre_ended.contains(&o.identity.id)
    }

    /// The team slots in use (D4, prelife-1c G10a): held observations, reservations, held pre-lifecycle folders.
    pub fn slots(&self) -> usize {
        let local = self.facts.local.iter().filter(|o| self.holds(o)).count();
        local + self.facts.reserved.len() + self.held.len()
    }

    /// Resolve only the mailbox reader; notices never determine current membership.
    /// The member named `name`: of every committed member with that name and no end notice that hc does not prove
    /// ended, exactly one, and hc shows it running. Its id and record; none otherwise. As in `member_target`, a
    /// member proven ended is no rival for its name; a lost or unknown one still is, so a member hc could not read
    /// never makes a same-name one look unique (#104 phase 1c).
    pub fn running_named(&self, name: &str) -> Option<(String, Value)> {
        let superseded = self.superseded();
        let candidates: Vec<_> = self
            .facts
            .local
            .iter()
            .filter(|o| o.identity.name == name)
            .filter(|o| !superseded.contains(&o.identity.id))
            .filter(|o| {
                self.records
                    .get(&o.identity.id)
                    .is_some_and(|s| s.committed && !s.noticed)
            })
            .filter(|o| o.life != super::lifecycle::Life::Ended)
            .collect();
        match candidates.as_slice() {
            [o] if o.life == super::lifecycle::Life::Running => self
                .records
                .get(&o.identity.id)
                .map(|s| (o.identity.id.clone(), s.record.clone())),
            _ => None,
        }
    }

    /// The members a committed resume superseded.
    pub fn superseded(&self) -> std::collections::BTreeSet<String> {
        let rows: Vec<_> = self
            .records
            .iter()
            .map(|(id, s)| (id.as_str(), &s.record, s.committed))
            .collect();
        superseded(&rows)
    }

    /// Member `id` as hc sees it now: running, its record committed and, unless `noticed_ok`, with no end notice.
    /// Anything else refuses 2 `parent`; another member's id is never this caller (#104 phase 1c).
    pub fn member(&self, id: &str, noticed_ok: bool) -> Res<registry::Me> {
        let observed = self.facts.local.iter().find(|o| o.identity.id == id);
        let running = observed.is_some_and(|o| o.life == super::lifecycle::Life::Running);
        match (observed, self.records.get(id)) {
            (Some(o), Some(stored))
                if running && stored.committed && (noticed_ok || !stored.noticed) =>
            {
                Ok(registry::Me {
                    name: o.identity.name.clone(),
                    key: id.to_string(),
                    record: Some(stored.record.clone()),
                })
            }
            _ => crate::error::refuse(2, "parent", "this agent cannot be proven to run"),
        }
    }

    pub fn reader(&self, caller: &Caller) -> Res<registry::Me> {
        // A member proven through its own hc terminal is itself by id, whichever pane shows it, or none (#104).
        // Notices never decide whether it may read its own mailbox.
        if let Some(id) = &caller.member {
            return self.member(id, true);
        }
        if let Some(error) = &self.view_error {
            return Err(error.clone());
        }
        let mine: Vec<_> = self
            .facts
            .local
            .iter()
            .filter(|o| {
                self.records[&o.identity.id]
                    .record
                    .get("pane")
                    .and_then(Value::as_str)
                    == Some(caller.pane.as_str())
            })
            .collect();
        match mine.as_slice() {
            [o] if o.life == super::lifecycle::Life::Running
                && self.records[&o.identity.id].committed
                && self
                    .views
                    .get(&o.identity.id)
                    .is_some_and(|p| p.pane == caller.pane) =>
            {
                Ok(registry::Me {
                    name: o.identity.name.clone(),
                    key: o.identity.id.clone(),
                    record: Some(self.records[&o.identity.id].record.clone()),
                })
            }
            [] if caller.agent_tag.is_empty() => Ok(registry::Me {
                name: "root".into(),
                key: super::mail::mailbox_key(&caller.pane, "")?,
                record: None,
            }),
            [] => crate::error::refuse(2, "parent", "this agent pane has no record"),
            _ => crate::error::refuse(
                2,
                "parent",
                "this pane's agent cannot be proven to run here",
            ),
        }
    }

    pub fn take(ctx: &Ctx, caller: &Caller) -> Res<Self> {
        let sc = scan(ctx, caller)?;
        let skip = sc.skip();
        let ids: Vec<_> = sc.local.iter().map(|l| l.identity.clone()).collect();
        let result = hc::connect(ctx).and_then(|core| {
            core_facts::take(&core, &team(caller), &ids, &skip, &sc.pre_addresses)
        });
        let (mut facts, error) = match result {
            Ok(facts) => (facts, None),
            Err(e) => (
                core_facts::Snapshot {
                    local: ids
                        .into_iter()
                        .map(|identity| core_facts::Observation {
                            identity,
                            terminal: None,
                            life: super::lifecycle::Life::Unknown,
                        })
                        .collect(),
                    reserved: Vec::new(),
                },
                Some(e),
            ),
        };
        settle(&mut facts, &sc.local); // R3: also when hc cannot be read
        sc.reserve(&mut facts); // b-calls 11: also when hc cannot be read
        Ok(Self::assemble(ctx, caller, sc, facts, error))
    }

    pub fn for_spawn(ctx: &Ctx, caller: &Caller, core: &Core<'_>) -> Res<Self> {
        let sc = scan(ctx, caller)?;
        let ids: Vec<_> = sc.local.iter().map(|l| l.identity.clone()).collect();
        let mut facts = core_facts::take(core, &team(caller), &ids, &sc.skip(), &sc.pre_addresses)?;
        settle(&mut facts, &sc.local);
        sc.reserve(&mut facts);
        Ok(Self::assemble(ctx, caller, sc, facts, None))
    }

    pub fn admit_name(
        &self,
        requested: Option<&str>,
        client: crate::parse::Client,
        task: &str,
    ) -> Res<String> {
        if let Some(e) = &self.core_error {
            return Err(e.clone());
        }
        if self.facts.count() >= 6 {
            return crate::error::refuse(2, "cap", "the session already has 6 live agents");
        }
        self.name_only(requested, client, task)
    }

    /// The name alone: in a team the limit is the tree's (D4), so no 0.2 cap.
    pub fn name_only(
        &self,
        requested: Option<&str>,
        client: crate::parse::Client,
        task: &str,
    ) -> Res<String> {
        if let Some(e) = &self.core_error {
            return Err(e.clone());
        }
        let names = self.facts.names();
        crate::checks::resolve_name(
            requested,
            client,
            task,
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    }

    fn assemble(
        ctx: &Ctx,
        caller: &Caller,
        scan: Scan,
        facts: core_facts::Snapshot,
        core_error: Option<Refusal>,
    ) -> Self {
        let Scan {
            local,
            held,
            pre_ended,
            pre,
            ..
        } = scan;
        let (rows, view_error) = match registry::rows(ctx, caller) {
            Ok(rows) => (rows, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let key = super::core_identity::Key::of(caller);
        let mut views = BTreeMap::new();
        let mut records = BTreeMap::new();
        for l in local {
            if let Some(view) = l.identity.view(&l.stored.record, &rows, &key) {
                views.insert(l.identity.id.clone(), view);
            }
            records.insert(l.identity.id, l.stored);
        }
        let view_order = rows
            .iter()
            .flat_map(|row| {
                views
                    .iter()
                    .filter(move |(_, p)| p.pane == row.pane)
                    .map(|(id, _)| id.clone())
            })
            .collect();
        Self {
            view_order,
            facts,
            records,
            views,
            core_error,
            view_error,
            held,
            pre_ended,
            pre,
        }
    }
}

#[cfg(test)]
mod tests;

/// The caller itself when it is a member proven through its own hc terminal: that member as hc sees it now, never
/// matched by pane, refused once it ended or has an end notice. None for a pane-proven caller (#104 phase 1c).
pub fn proven_self(ctx: &Ctx, caller: &Caller) -> Option<Res<registry::Me>> {
    let id = caller.member.as_ref()?;
    Some(Snapshot::take(ctx, caller).and_then(|s| s.member(id, false)))
}
