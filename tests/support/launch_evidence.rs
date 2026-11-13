//! Preserve launch artifacts when a live view read panics.

use std::path::Path;

pub fn checked<T>(folder: &Path, fixture: &Path, read: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(read)).map_err(|panic| {
        let reason = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "view read panicked".into());
        let mut files = vec![folder.join("launcher.json"), folder.join("abort.json")];
        for dir in [folder, fixture] {
            if let Ok(entries) = std::fs::read_dir(dir) {
                files.extend(entries.flatten().map(|e| e.path()).filter(|p| {
                    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                        (n.starts_with("client.") && n.ends_with(".json"))
                            || n.starts_with("exited.")
                    })
                }));
            }
        }
        files.sort();
        files.dedup();
        let mut out = reason;
        for file in files {
            let text =
                std::fs::read_to_string(&file).unwrap_or_else(|e| format!("unavailable: {e}"));
            out.push_str(&format!("\n{}\n{text}", file.display()));
        }
        out
    })
}
