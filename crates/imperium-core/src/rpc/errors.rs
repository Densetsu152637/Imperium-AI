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
