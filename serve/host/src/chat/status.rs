//! The status words on a user's queued message. Apps read the serialized values, so they must not change.

use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RequestStatus {
    Queued,
    Sent,
    Failed,
    Cancelled,
}
