//! A guest's doors (kernel/lib/kith_door.py): fixed commands kith-permit runs for a member in a sandbox of their own,
//! such as video's render. Each running door leaves a claim; `kith door sweep` kills the doors of the listed people,
//! every door whose supervisor is gone (a kith-permit or host killed with -9), and every door its person may no longer
//! run (D1). The host sweeps when it cuts someone off or narrows their access, at start, and every minute.

use std::time::Duration;

const EVERY: Duration = Duration::from_secs(60);

/// `kith door sweep [--person P]...`: `persons` lose every door; the rest are checked again against kith-auth.
pub async fn sweep(persons: &[String]) {
    let Some(bin) = crate::remote::account::bin().await.map(|b| b.with_file_name("kith-door")) else { return };
    let mut args = vec!["sweep"];
    for p in persons {
        args.extend(["--person", p.as_str()]);
    }
    match crate::channels::hq::output(&bin, &args).await {
        Some(out) if out.status.success() => {
            let ended: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
            if ended["ended"].as_array().is_some_and(|e| !e.is_empty()) {
                tracing::info!(claims = %ended["ended"], "ended doors");
            }
        }
        Some(out) => tracing::warn!(why = %String::from_utf8_lossy(&out.stderr).trim(), "kith door sweep failed"),
        None => tracing::debug!("kith door sweep didn't run"),
    }
}

/// At start (doors a killed host or kith-permit left), then every minute.
pub async fn sweep_loop() {
    loop {
        sweep(&[]).await;
        tokio::time::sleep(EVERY).await;
    }
}
