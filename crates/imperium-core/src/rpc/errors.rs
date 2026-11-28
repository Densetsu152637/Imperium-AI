use super::*;

pub(super) fn stopped(error: imperium_engine::SessionClosed) -> ErrorObject {
    ErrorObject::new(codes::INTERNAL_ERROR, error.to_string())
}

pub(super) fn queue_error(error: QueueError) -> ErrorObject {
    let code = match error {
        QueueError::Closed => codes::INTERNAL_ERROR,
        QueueError::UnknownAgent(_)
        | QueueError::UnknownMessage(_)
        | QueueError::Empty
        | QueueError::Steering(_) => codes::INVALID_PARAMS,
    };
    ErrorObject::new(code, error.to_string())
}

pub(super) fn archive_error(error: crate::state::ArchiveError) -> ErrorObject {
    use crate::state::ArchiveError;
    let code = match &error {
        ArchiveError::NotFound(_) => codes::SESSION_NOT_FOUND,
        ArchiveError::Persistent(_) => codes::INVALID_PARAMS,
        ArchiveError::Record(_) | ArchiveError::Resume(_) => codes::INTERNAL_ERROR,
    };
    ErrorObject::new(code, error.to_string())
}

pub(super) fn switch_error(error: SwitchError) -> ErrorObject {
    let code = match &error {
        SwitchError::Busy => codes::SESSION_BUSY,
        SwitchError::TooLarge { .. } | SwitchError::NoFit { .. } => codes::CONTEXT_TOO_LARGE,
        SwitchError::UnknownWindow(_) | SwitchError::NoModel => codes::INVALID_PARAMS,
        SwitchError::Unavailable(_) | SwitchError::Summary(_) => codes::MODEL_UNAVAILABLE,
        SwitchError::Closed => codes::INTERNAL_ERROR,
    };
    ErrorObject::new(code, error.to_string())
}

/// `-32010`: the daemon runs as a server (`daemon.server`) and `feature` (one of
/// `imperium_protocol::jsonrpc::server_features`) is unavailable there.
pub(crate) fn unavailable_on_server(feature: &'static str) -> ErrorObject {
    use imperium_protocol::jsonrpc::server_features as f;
    let message = match feature {
        f::PLUGIN_APPROVAL => {
            "plugin approvals are unavailable on a server (daemon.server): approve the plugin in \
             the desktop app on its own machine; approvals carried over in the data folder still \
             count"
        }
        f::AUTO_MERGE_ENABLE => {
            "turning auto-merge on is unavailable on a server (daemon.server): only the desktop \
             app on its own machine may; turning it off works from any client"
        }
        f::TERMINAL_GRANT => {
            "this daemon runs as a server (daemon.server), which counts no trust or prompt grant \
             made at a terminal"
        }
        f::SESSION_ERASE => {
            "erasing a session is unavailable on a server (daemon.server): only the desktop app              on its own machine may; archive the session instead"
        }
        f::CONTAINER_TIER => imperium_config::CONTAINER_TIER_ON_SERVER,
        _ => "unavailable on a server (daemon.server)",
    };
    unavailable_on_server_with(feature, message)
}

/// [`unavailable_on_server`] with its own `message`.
pub(crate) fn unavailable_on_server_with(
    feature: &'static str,
    message: impl Into<String>,
) -> ErrorObject {
    ErrorObject {
        data: Some(json!({ "feature": feature })),
        ..ErrorObject::new(codes::UNAVAILABLE_ON_SERVER, message)
    }
}

pub(super) fn incompatible(client: ProtocolVersion) -> ErrorObject {
    ErrorObject {
        data: Some(json!({ "daemon": PROTOCOL_VERSION })),
        ..ErrorObject::new(
            codes::INCOMPATIBLE_PROTOCOL,
            format!(
                "client protocol {client} is incompatible with daemon protocol {PROTOCOL_VERSION}"
            ),
        )
    }
}
