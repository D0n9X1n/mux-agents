//! a3c call 47: one validator per identity kind read from evidence (an hc name, an hc address, a member id). An
//! empty, missing or malformed identity never matches another one: a comparison is a match only when both sides pass
//! the kind's validator and are equal. A recorded identity that fails refuses `evidence` and has no effect.
use crate::error::Refusal;

/// An hc terminal name (launch-1 G2): `m` and 8 lowercase hex digits.
pub fn hc_name(name: &str) -> bool {
    super::core_identity::is_hc_name(name)
}

/// An hc terminal address: non-empty, with no whitespace or control character. hc owns the format.
pub fn hc_address(address: &str) -> bool {
    !address.is_empty() && !address.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// A member id: 12 lowercase hex (`agents::agent_id`).
pub fn member_id(id: &str) -> bool {
    super::agent_id(id)
}

/// A match: both sides present, both valid by `valid`, and equal. Two empty or missing sides never match.
pub fn same(valid: fn(&str) -> bool, a: Option<&str>, b: Option<&str>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if valid(a) && valid(b) && a == b)
}

/// The refusal for a recorded identity that fails its validator: `evidence`, nothing done.
pub fn refused(what: &str) -> Refusal {
    Refusal {
        code: 1,
        check: "evidence",
        message: format!("the recorded {what} is missing or invalid; nothing was done"),
        reason: None,
        leftover: None,
        details: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_validator_rejects_empty_and_malformed_values() {
        assert!(hc_name("m0123abcd"));
        for bad in [
            "",
            "m",
            "m0123ABCD",
            "m0123abc",
            "x0123abcd",
            "../x",
            "m0123abc ",
        ] {
            assert!(!hc_name(bad), "hc name {bad:?}");
        }
        assert!(hc_address("t1"));
        for bad in ["", " ", "t 1", "t1\n", "\0"] {
            assert!(!hc_address(bad), "hc address {bad:?}");
        }
        assert!(member_id("0123456789ab"));
        for bad in [
            "",
            "0123456789a",
            "0123456789AB",
            "../456789ab0",
            "0123456789abc",
        ] {
            assert!(!member_id(bad), "member id {bad:?}");
        }
    }

    #[test]
    fn only_two_equal_valid_sides_match() {
        assert!(same(hc_address, Some("t1"), Some("t1")));
        for (a, b) in [
            (Some(""), Some("")),
            (None, None),
            (Some(""), None),
            (None, Some("")),
            (Some("t 1"), Some("t 1")),
            (Some("t1"), Some("t2")),
            (Some("t1"), None),
        ] {
            assert!(!same(hc_address, a, b), "{a:?} {b:?}");
        }
        assert!(!same(hc_name, Some(""), Some("")));
        assert!(!same(member_id, Some(""), Some("")));
    }

    #[test]
    fn an_invalid_recorded_identity_refuses_evidence() {
        let r = refused("hcName");
        assert_eq!((r.check, r.code), ("evidence", 1));
        assert!(r.message.contains("hcName"), "{}", r.message);
    }
}
