use crate::tuning::PROTOCOL_VERSION;

pub(crate) const BLOCKED_UNFINISHED: &str = "UPDATE_BLOCKED_UNFINISHED_TRANSFERS";
pub(crate) const BLOCKED_ACTIVE: &str = "UPDATE_BLOCKED_ACTIVE_TRANSFERS";

pub(crate) fn requires_idle_transfers(remote_protocol_version: Option<u16>) -> bool {
    remote_protocol_version != Some(PROTOCOL_VERSION)
}

pub(crate) fn has_blocking_transfers<'a>(
    statuses: impl IntoIterator<Item = &'a str>,
    requires_idle: bool,
) -> bool {
    statuses.into_iter().any(|status| {
        let terminal = matches!(
            status,
            "completed" | "cancelled" | "rejected" | "partial_completed" | "incompatible"
        );
        if terminal {
            return false;
        }
        if requires_idle {
            return true;
        }
        !matches!(status, "paused" | "remote_paused")
    })
}
