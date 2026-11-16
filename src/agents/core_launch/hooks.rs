use std::cell::RefCell;
pub(crate) type After = Box<dyn Fn(&str)>;
thread_local! {
    static AFTER: RefCell<Option<After>> = const { RefCell::new(None) };
}
/// Runs `f` with the step name after every forward step.
pub(crate) fn after(f: Option<After>) {
    AFTER.with(|a| *a.borrow_mut() = f);
}
pub(super) fn fire(step: &str) {
    AFTER.with(|a| {
        if let Some(f) = a.borrow().as_ref() {
            f(step);
        }
    });
}
/// Clears the hook and every fault when the test ends.
pub(crate) struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        after(None);
        crate::state::fs::fail_durable_flush(None);
        crate::state::fs::fail_sync_dir(None);
        crate::state::fs::fail_sync_file(None);
    }
}
