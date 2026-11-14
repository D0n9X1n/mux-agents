//! `mux-agent hold <release file>`: the first window of a session `up` creates; exits 0 once the file exists
//! (checked every 200 ms) or after 10 minutes (#103). Each check counts what the clock moved, never less than POLL.

use std::{path::Path, time::Duration};

use crate::ctx::Clock;

pub const POLL: Duration = Duration::from_millis(200);
pub const LIMIT: Duration = Duration::from_secs(600);

pub fn wait(release: &Path, clock: &dyn Clock, limit: Duration) {
    let (mut waited, mut last) = (Duration::ZERO, clock.now());
    while !release.exists() && waited < limit {
        clock.sleep(POLL);
        let now = clock.now();
        waited += now.duration_since(last).unwrap_or_default().max(POLL);
        last = now;
    }
}

/// `hold <absolute path>` exactly: Some(0) after the wait; None otherwise, so the verb table answers it.
pub fn main(args: &[String], clock: &dyn Clock) -> Option<u8> {
    let [verb, path] = args else { return None };
    (verb == "hold" && Path::new(path).is_absolute())
        .then(|| wait(Path::new(path), clock, limit()))?;
    Some(0)
}

fn limit() -> Duration {
    #[cfg(feature = "test-panic")]
    if let Some(ms) = std::env::var("MUX_AGENTS_HOLD_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return Duration::from_millis(ms);
    }
    LIMIT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_checks_every_poll_until_its_limit_or_the_file() {
        let fakes = crate::testing::Fakes::default();
        let (ctx, d) = (fakes.ctx(), crate::testing::TempDir::new());
        let release = d.path().join("r.release");
        wait(&release, ctx.clock.as_ref(), Duration::from_secs(1));
        assert_eq!(fakes.slept().len(), 5);
        std::fs::write(&release, "").unwrap();
        wait(&release, ctx.clock.as_ref(), LIMIT);
        assert_eq!(fakes.slept().len(), 5);
        assert_eq!(
            main(&["hold".into(), "relative".into()], ctx.clock.as_ref()),
            None
        );
    }
}
