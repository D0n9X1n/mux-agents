//! views-6f V4-V6: a kept shell is only its hc labels (`mux.kept=<T>`, `mux.keptName=<name>`, group B R1.3). This
//! module lists and closes them; it writes no file (6f-R4).
use crate::error::{Res, refuse};
use crate::hc::{Core, State, Terminal};

pub const KEPT: &str = "mux.kept";
pub const KEPT_NAME: &str = "mux.keptName";

/// One row: a terminal hc shows live with `mux.kept=<T>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    pub address: String,
    pub name: String,
}

fn ours(t: &Terminal, team: &str) -> bool {
    t.state == State::Live && t.labels.get(KEPT).map(String::as_str) == Some(team)
}

/// Ambiguity 11, v6f calls 3, 7: one live `hc ls --label mux.kept=<T>` (hc filters the label; finished shells are
/// not listed), then one `get` per row for its name: hc 0.1.21's `ls` rows carry no labels and no name, so
/// `mux.keptName` needs the `get`. The `get` also re-checks live and `mux.kept=<T>`; a shell gone or finished in
/// between is skipped; another hc failure fails the list. Sorted by name, then address.
pub fn list(core: &Core<'_>, team: &str) -> Res<Vec<Shell>> {
    let mut shells = Vec::new();
    for address in core.ls_live(&[(KEPT, team)])? {
        let t = match core.get(&address) {
            Err(e) if e.check == "not_found" => continue,
            other => other?,
        };
        if !ours(&t, team) {
            continue;
        }
        let name = t
            .labels
            .get(KEPT_NAME)
            .filter(|n| !n.is_empty())
            .cloned()
            .unwrap_or_else(|| address.clone());
        shells.push(Shell { address, name });
    }
    shells.sort_by(|a, b| (&a.name, &a.address).cmp(&(&b.name, &b.address)));
    Ok(shells)
}

/// Ambiguity 12, 6f-R3: one re-read immediately before the kill, nothing between them.
pub fn close(core: &Core<'_>, team: &str, address: &str) -> Res<()> {
    let t = match core.get(address) {
        Err(e) if e.check == "not_found" => return refuse(2, "target", "no such kept shell"),
        other => other?,
    };
    if t.state == State::Finished {
        return refuse(2, "target", "the shell has already finished");
    }
    if !ours(&t, team) {
        return refuse(2, "target", "that shell is not a kept shell of this team");
    }
    core.kill(address)
}
