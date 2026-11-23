//! The environment variables that let a process load code of someone else's choosing, which the
//! daemon refuses in the desktop app's environment before it counts a plugin approval (check 4,
//! docs/06-extensibility.md#plugin-marketplaces). The daemon reads a peer's environment
//! where the OS lets it (Linux, macOS); on Windows it cannot, so the app applies the same list to
//! its own environment before it asks for an approval. One list, here, so the two never differ.

/// The first variable in `names` whose presence refuses a peer: any `WEBVIEW2_*` or `DYLD_*`, and
/// `LD_PRELOAD`, `LD_LIBRARY_PATH` and `LD_AUDIT`. Names compare ignoring case only on Windows, as
/// Windows does; elsewhere the loader honours only these exact names, and macOS lists lowercase
/// strings of its own (`dyld_file=`, from the kernel's `apple[]` vector) next to the environment.
pub fn refused_variable<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<String> {
    names.into_iter().find_map(|name| {
        let upper = if cfg!(windows) {
            name.to_ascii_uppercase()
        } else {
            name.to_string()
        };
        let refused = upper.starts_with("WEBVIEW2_")
            || upper.starts_with("DYLD_")
            || matches!(
                upper.as_str(),
                "LD_PRELOAD" | "LD_LIBRARY_PATH" | "LD_AUDIT"
            );
        refused.then(|| name.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_and_webview_overrides_are_refused() {
        assert_eq!(refused_variable(["PATH", "HOME"]), None);
        for name in [
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "LD_AUDIT",
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
            "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER",
            "DYLD_INSERT_LIBRARIES",
        ] {
            assert_eq!(refused_variable(["PATH", name]).as_deref(), Some(name));
        }
        // Case matters except on Windows, where names compare ignoring it.
        let lower = refused_variable(["webview2_user_data_folder", "dyld_file"]);
        if cfg!(windows) {
            assert_eq!(lower.as_deref(), Some("webview2_user_data_folder"));
        } else {
            assert_eq!(lower, None);
        }
    }
}
