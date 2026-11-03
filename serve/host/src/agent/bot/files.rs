//! File copies run outside the actor; publication still belongs to its original turn.
use super::Actor;
use crate::chat::files::{self, Snapshot};
use crate::store::EntryKind;
use anyhow::{Result, anyhow, ensure};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{mpsc, oneshot};

type Reply = oneshot::Sender<Result<()>>;
pub(super) struct Completed {
    epoch: u64,
    cancelled: Arc<AtomicBool>,
    reply: Reply,
    result: Result<Vec<Snapshot>>,
}

pub(super) struct FileShares {
    epoch: u64,
    pub summary: Option<String>,
    pending: Vec<Arc<AtomicBool>>,
    /// What each pending share was, by its cancel flag: told to the person when an off-turn share fails.
    labels: Vec<(Arc<AtomicBool>, String)>,
    pub tx: mpsc::UnboundedSender<Completed>,
    pub rx: mpsc::UnboundedReceiver<Completed>,
}
impl Default for FileShares {
    fn default() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self { epoch: 0, summary: None, pending: Vec::new(), labels: Vec::new(), tx, rx }
    }
}
impl FileShares {
    /// Whether no share is still being sent.
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn cancel(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        for flag in self.pending.drain(..) {
            flag.store(true, Ordering::Release);
        }
    }
}
impl Drop for FileShares {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Actor {
    pub(super) fn begin_file(
        &mut self,
        sources: Vec<String>,
        name: Option<String>,
        cancelled: Arc<AtomicBool>,
        reply: Reply,
    ) {
        let label = super::offturn::file_label(&sources, name.as_deref());
        if let Err(error) = self.can_send_file() {
            self.share_failed(&label, &error);
            let _ = reply.send(Err(error));
            return;
        }
        self.files.pending.retain(|flag| !flag.load(Ordering::Acquire));
        if self.files.pending.len() >= 4 {
            let error = anyhow!("too many pending file shares");
            self.share_failed(&label, &error);
            let _ = reply.send(Err(error));
            return;
        }
        self.files.pending.push(cancelled.clone());
        self.files.labels.push((cancelled.clone(), label));
        let epoch = self.files.epoch;
        let root = files::root(&self.cfg.id);
        // Relative paths are the agent's own: it works in the launch's folder (a project-pinned place), not the bot's.
        let cwd = PathBuf::from(self.conn_started_as.as_ref().map_or(self.cfg.cwd.as_str(), |s| s.1.as_str()));
        let folders: Vec<PathBuf> =
            std::iter::once(cwd.clone()).chain(self.conn_dirs.iter().map(PathBuf::from)).collect();
        let tx = self.files.tx.clone();
        tokio::spawn(async move {
            let flag = cancelled.clone();
            let result = tokio::task::spawn_blocking(move || {
                let reach = files::Reach::new(folders, crate::cards::resource::roots());
                files::snapshot_all(&root, &cwd, &sources, name.as_deref(), &reach, &flag)
            })
            .await;
            let result = result.map_err(anyhow::Error::from).and_then(std::convert::identity);
            let _ = tx.send(Completed { epoch, cancelled, reply, result });
        });
    }

    /// A share that didn't happen. Out of a turn the person started nobody reads the agent's tool result, so the
    /// chat says so (see `offturn`).
    fn share_failed(&self, label: &str, error: &anyhow::Error) {
        if self.off_turn.is_some() {
            self.off_turn_failed(label, error);
        }
    }

    fn can_send_file(&self) -> Result<i64> {
        ensure!(!self.stop_requested, "turn is stopping");
        ensure!(
            self.active_group.is_none() && self.active_request.is_none() && self.active_routine.is_none(),
            "send_file is only available in the bot's own chat and threads"
        );
        self.turn.ok_or_else(|| anyhow!("no turn is running"))
    }

    pub(super) fn finish_file(&mut self, completed: Completed) {
        self.files.pending.retain(|flag| !Arc::ptr_eq(flag, &completed.cancelled));
        let label = self
            .files
            .labels
            .iter()
            .position(|(flag, _)| Arc::ptr_eq(flag, &completed.cancelled))
            .map(|i| self.files.labels.swap_remove(i).1);
        let withdrawn = completed.epoch != self.files.epoch || completed.cancelled.load(Ordering::Acquire);
        let result = (|| {
            ensure!(
                completed.epoch == self.files.epoch
                    && !completed.cancelled.load(Ordering::Acquire)
                    && !completed.reply.is_closed(),
                "file sharing cancelled"
            );
            let turn = self.can_send_file()?;
            ensure!(self.hub.store.bot(&self.cfg.id)?.is_some_and(|bot| !bot.deleted), "bot was deleted");
            let mut snapshots = completed.result?;
            let text = match snapshots.as_slice() {
                [one] => format!("{} ({} bytes)", one.meta.name, one.meta.size),
                many => format!(
                    "{} files: {}",
                    many.len(),
                    many.iter().map(|s| s.meta.name.as_str()).collect::<Vec<_>>().join(", ")
                ),
            };
            let metas: Vec<_> = snapshots.iter().map(|s| &s.meta).collect();
            self.close_seg();
            let entry = self
                .add(EntryKind::Agent, turn, json!({"text": text, "final": true, "files": metas}))
                .ok_or_else(|| anyhow!("couldn't save the file message"))?;
            self.last_agent = Some(entry.id);
            // Only now, with the entry saved, do the copies become permanent.
            snapshots.iter_mut().for_each(Snapshot::commit);
            self.files.summary = Some(text);
            self.set_activity("Working…");
            Ok(())
        })();
        if let (Err(error), Some(label), false, true) = (&result, label, withdrawn, self.off_turn_context()) {
            self.off_turn_failed(&label, error);
        }
        let _ = completed.reply.send(result);
    }
}
