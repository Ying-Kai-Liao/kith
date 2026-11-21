//! Remote screen: the phone views and controls this computer over WebRTC.
//!
//! Capture, encoding and the phone's input live in a separate *screen helper*
//! that connects to `~/.kith/serve/screen.sock` — on macOS `KithScreen.app`
//! (a signed bundle, so Screen Recording / Accessibility grants stick), on
//! Linux `kith-screen` (xdg portals + `GStreamer`). The host stays the only
//! network-facing process: it relays WebRTC signaling and gates access (on by
//! default where there is a desktop, toggled only from this computer).
//!
//! Helper protocol: newline-delimited JSON-RPC 2.0 over the socket.
//! - helper → host notifications
//!   - `status` [`HelperStatus`]
//!   - `session {session, state: "connected" | "closed"}`
//! - host → helper requests
//!   - `answer {session, sdp, display?, iceServers, maxBitrateBps, maxFramerate}` → `{sdp}`:
//!     non-trickle ICE, with short-lived STUN/TURN credentials for remote connections.
//!   - `close {session}`, `closeAll {}`
//!
//! The phone's input travels straight to the helper over WebRTC data channels
//! (`input-fast` unordered for moves, `input` reliable): `move`, `click`, `drag`, `scroll`,
//! `text`, `key` in display points, plus `down`/`up` for touch drags and `clipboard {text}`.

mod helper;
mod permissions;
mod protocol;
mod viewer;

pub use helper::serve_helpers;
#[cfg(target_os = "linux")]
pub use helper::supervise_linux_helper;
pub use viewer::{IceConfig, local_ice, watch_viewer};

use crate::LockExt;
use anyhow::{Result, anyhow, bail};
use helper::{Link, graphical_session};
use protocol::HelperStatus;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use tokio::sync::broadcast;

const HELPER_TIMEOUT: Duration = Duration::from_secs(15);
const KV_ENABLED: &str = "screen_enabled";

#[derive(Default)]
struct Control {
    /// The phone took over: bots may only look.
    user: bool,
}

struct Viewer {
    owner: String,
    ice: IceConfig,
    started: bool,
    pending_until: i64,
}

pub struct Screen {
    /// The user's choice; `None` until they make one (see [`Screen::enabled`]).
    enabled: Mutex<Option<bool>>,
    events: broadcast::Sender<Value>,
    link: Mutex<Option<Arc<Link>>>,
    status: Mutex<HelperStatus>,
    control: Mutex<Control>,
    sessions: Mutex<HashMap<String, Viewer>>,
    #[cfg_attr(windows, expect(dead_code, reason = "Windows has no screen helper yet"))]
    next_link: AtomicU64,
}

impl Screen {
    pub fn new(enabled: Option<bool>, events: broadcast::Sender<Value>) -> Self {
        Self {
            enabled: Mutex::new(enabled),
            events,
            link: Mutex::default(),
            status: Mutex::default(),
            control: Mutex::default(),
            sessions: Mutex::default(),
            next_link: AtomicU64::new(1),
        }
    }

    pub fn load_enabled(store: &crate::store::Store) -> Option<bool> {
        match store.kv_get(KV_ENABLED).as_deref() {
            Some("1") => Some(true),
            Some("0") => Some(false),
            _ => None,
        }
    }

    /// On by default wherever there is a screen to share: always on macOS, and on Linux
    /// only while a graphical session is up (checked live, so a headless server stays off
    /// and a desktop that logs in after the host started turns on).
    pub fn enabled(&self) -> bool {
        (*self.enabled.locked()).unwrap_or_else(graphical_session)
    }

    /// What clients show: availability, permissions, displays, who's in control.
    pub fn state(&self) -> Value {
        let st = self.status.locked().clone();
        let connected = self.link.locked().is_some();
        let viewers = self.sessions.locked().values().filter(|v| v.started).count();
        let c = self.control.locked();
        json!({
            "enabled": self.enabled(),
            "connected": connected,
            "platform": st.platform,
            "permissionApp": st.permission_app,
            "capture": st.capture,
            "input": st.input,
            "displays": st.displays,
            "userControl": c.user,
            // Bots no longer operate the computer; older clients still read these two.
            "agentBot": null,
            "computerUse": false,
            "viewers": viewers,
        })
    }

    fn emit(&self) {
        let _ = self.events.send(json!({"type": "screen", "screen": self.state()}));
    }

    fn link(&self) -> Result<Arc<Link>> {
        if !self.enabled() {
            bail!("Remote screen is turned off on this computer. Turn it on in Kith's menu there.");
        }
        self.link.locked().clone().ok_or_else(|| anyhow!("The screen helper isn't running on this computer."))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    pub(super) fn retina() -> protocol::Display {
        protocol::Display { id: 1, name: "Built-in".into(), width: 1512.0, height: 982.0, main: true }
    }
}
