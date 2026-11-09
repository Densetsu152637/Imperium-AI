//! The page UI mod frames load from. The app's content security policy allows no inline
//! scripts, and a `srcdoc` frame inherits it, so a mod could never run there. Instead each frame
//! loads this page from the `imperium-mod` protocol, which carries a policy of its own (inline
//! scripts and `data:` modules, nothing from the network), and the app hands it the frame's
//! document over postMessage. The frame stays sandboxed with an opaque origin, so the mod still
//! cannot reach the app, the daemon token, or other mods.

use std::borrow::Cow;

use tauri::http::{Request, Response, header};

pub const SCHEME: &str = "imperium-mod";

/// What a mod frame may load: its own inline scripts, styles and `data:` modules and images.
const POLICY: &str = "default-src 'none'; script-src 'unsafe-inline' data:; \
    style-src 'unsafe-inline'; img-src data: blob:; font-src data:";

/// Waits for the document from the app (only from the window that embeds it), then becomes it.
const PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"></head><body><script>
addEventListener("message", function load(message) {
  if (message.source !== parent || !message.data || message.data.kind !== "imperium.frame") return;
  removeEventListener("message", load);
  document.open();
  document.write(message.data.html);
  document.close();
});
parent.postMessage({ kind: "imperium.frame-ready" }, "*");
</script></body></html>"#;

/// Serves the frame page for any path; there is nothing else on the protocol.
pub fn serve(_request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_SECURITY_POLICY, POLICY)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Cow::Borrowed(PAGE.as_bytes()))
        .expect("a fixed response is always valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_page_carries_its_own_policy() {
        let response = serve(Request::new(Vec::new()));
        let policy = response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap();
        assert!(policy.contains("script-src 'unsafe-inline' data:"));
        assert!(policy.starts_with("default-src 'none'"));
        assert!(
            std::str::from_utf8(response.body())
                .unwrap()
                .contains("imperium.frame-ready")
        );
    }
}
