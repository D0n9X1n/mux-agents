//! Typed refusals: every failure leaves as one code, one check name and a fixed message.

use std::io;

/// Code 1 (cannot run) or 2 (usage or policy), a short check name and a message safe to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: u8,
    pub check: &'static str,
    pub message: String,
}

pub type Res<T> = Result<T, Refusal>;

pub fn refuse<T>(code: u8, check: &'static str, message: impl Into<String>) -> Res<T> {
    Err(Refusal {
        code,
        check,
        message: message.into(),
    })
}

impl Refusal {
    /// 1 `internal` with a fixed reason, worded like the io and panic ones.
    pub fn internal(reason: &str) -> Refusal {
        Refusal {
            code: 1,
            check: "internal",
            message: format!("unexpected error ({reason})"),
        }
    }
}

impl From<io::Error> for Refusal {
    // The kind only: an io message can carry paths or other text the reply must not echo.
    fn from(error: io::Error) -> Self {
        Refusal {
            code: 1,
            check: "internal",
            message: format!("unexpected error ({:?})", error.kind()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn refusal_keeps_code_check_message() {
        let refused: Res<()> = refuse(2, "name", "taken");
        let refusal = refused.unwrap_err();
        assert_eq!(
            (refusal.code, refusal.check, refusal.message.as_str()),
            (2, "name", "taken")
        );

        let io: Refusal = io::Error::new(io::ErrorKind::PermissionDenied, "SECRET").into();
        assert_eq!((io.code, io.check), (1, "internal"));
        assert_eq!(io.message, "unexpected error (PermissionDenied)");
        assert!(!io.message.contains("SECRET"));
    }
}
