//! The user driving a session's browser (`session.browser_control`, `session.browser_act`):
//! the Browser panel takes the agent's browser over, acts in it, and gives it back. While the
//! user drives, the agent's browser tools are refused (see `imperium_web::Takeover`).

use std::sync::Arc;

use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::{
    BrowserActParams, BrowserActResult, BrowserControlParams, BrowserControlResult, BrowserPage,
};
use imperium_protocol::session::SessionEvent;
use imperium_web::BrowserSetup;
use imperium_web::browser::View;

use crate::DaemonState;

fn page(view: View) -> BrowserPage {
    BrowserPage {
        url: view.url,
        title: view.title,
        screenshot: view.screenshot,
    }
}

fn view_event(page: &BrowserPage) -> SessionEvent {
    SessionEvent::BrowserView {
        url: page.url.clone(),
        title: page.title.clone(),
        screenshot: page.screenshot.clone(),
    }
}

/// The session's browser; browser tools being off is the one way it can be missing.
fn browser_of(state: &DaemonState, session_id: &str) -> Result<BrowserSetup, ErrorObject> {
    if state.session(session_id).is_none() {
        return Err(ErrorObject::new(
            codes::SESSION_NOT_FOUND,
            format!("no session {session_id}"),
        ));
    }
    state.browser_of(session_id).ok_or_else(|| {
        ErrorObject::new(
            codes::INVALID_PARAMS,
            "this session has no browser (web.browser is off or no Chrome, Chromium, or Edge was found)",
        )
    })
}

/// `session.browser_control`.
pub async fn control(
    state: Arc<DaemonState>,
    params: BrowserControlParams,
) -> Result<BrowserControlResult, ErrorObject> {
    let browser = browser_of(&state, &params.session_id)?;
    let session = state
        .session(&params.session_id)
        .ok_or_else(|| ErrorObject::new(codes::SESSION_NOT_FOUND, "the session has ended"))?;
    if !params.user {
        if browser.give_back() {
            session.notify(SessionEvent::BrowserControl { user: false });
        }
        return Ok(BrowserControlResult { view: None });
    }
    let changed = !browser.takeover.user_driving();
    let view = browser
        .take_over()
        .await
        .map(page)
        .map_err(|message| ErrorObject::new(codes::INTERNAL_ERROR, message))?;
    if changed {
        session.notify(SessionEvent::BrowserControl { user: true });
    }
    session.notify(view_event(&view));
    Ok(BrowserControlResult { view: Some(view) })
}

/// `session.browser_act`.
pub async fn act(
    state: Arc<DaemonState>,
    params: BrowserActParams,
) -> Result<BrowserActResult, ErrorObject> {
    let browser = browser_of(&state, &params.session_id)?;
    let view = browser
        .user_act(&params.action)
        .await
        .map(page)
        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))?;
    if let Some(session) = state.session(&params.session_id) {
        session.notify(view_event(&view));
    }
    Ok(BrowserActResult { view })
}
