//! The queue: messages sent while an agent works wait in the thread's inbox and go to it together as one prompt
//! when the run ends. With a Jev key each is routed by Jev first: "stop" kills the run, a correction stops it and
//! starts it again with the correction merged in, a different subject becomes its own turn, anything else is
//! merged; without Jev, or when Jev fails, it is merged.
//!
//! The Python bridge ran on one thread, so a stretch of code without an `await` could not be interrupted. Here such
//! a stretch is one `Core::with` closure (the state lock is never held across an await).

use super::adapter::{Adapter, Ended, Incoming, Reaction, StopPress};
use super::core::{Busy, Core};
use super::hq;
use super::text::{INTERRUPTED, merged, short};
use crate::LockExt as _;
use crate::chat::router::Route;
use serde_json::Value;
use std::sync::Arc;

/// Kills the agent's whole process group (hq -> claude -> its tools).
#[cfg(unix)]
pub(super) fn kill(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: killpg only sends a signal; the group of a process this host started.
        unsafe { libc::killpg(pid, libc::SIGTERM) };
    }
}

#[cfg(not(unix))]
pub(super) fn kill(_pid: u32) {}

/// What a card's wake came to (see [`Core::wake`]).
#[derive(Debug, PartialEq, Eq)]
pub enum Wake {
    Queued,
    /// Someone else already took this card's resume.
    Claimed,
    /// The thread is gone from the channel state: the resume can't happen.
    NoThread,
    /// The connection isn't up (yet): try again later.
    Offline,
}

/// Ends a worker's claim on its work key, also when the worker panics or fails.
struct Claim {
    core: Arc<Core>,
    wk: String,
    typing: tokio::task::JoinHandle<()>,
    done: bool,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.typing.abort();
        if !self.done {
            // Keep the inbox on disk; the next message or restart picks it up.
            self.core.with(|i| {
                i.busy.remove(&self.wk);
                if i.st.inbox.get(&self.wk).is_some_and(Vec::is_empty) {
                    i.st.inbox.remove(&self.wk);
                }
                i.save();
            });
        }
    }
}

impl Core {
    pub(super) async fn route(&self, task: &str, recent: &[String], latest: &str) -> crate::chat::router::Verdict {
        if !self.settings.router {
            return crate::chat::router::Verdict { route: Route::Merge, hold: false, path: "router off".into() };
        }
        self.judge.route(task, recent, latest, self.settings.interrupt).await
    }

    fn remember(&self, wk: &str, m: &Incoming) {
        self.with(|i| {
            let recent = i.history.entry(wk.to_owned()).or_default();
            recent.push(super::text::last_chars(&m.text, 1000).to_owned());
            let excess = recent.len().saturating_sub(6);
            recent.drain(..excess);
        });
    }

    pub(super) async fn deliver(
        self: &Arc<Self>,
        ad: &Arc<dyn Adapter>,
        key: &str,
        agent: &str,
        m: &Incoming,
        task: String,
    ) {
        let wk = format!("{key}#{agent}");
        let running = self.with(|i| i.busy.get(&wk).filter(|b| b.running()).map(|b| (b.pid, b.task.clone())));
        let Some((pid, running_task)) = running else {
            // Nothing running: the message joins the prompt about to go (or starts one) right away, so the run
            // can't leave without it while Jev answers; Jev is then only asked whether it says "stop".
            let (spawn, waiting, recent) = self.with(|i| {
                let was_busy = i.busy.contains_key(&wk);
                let q = i.st.inbox.entry(wk.clone()).or_default();
                let queued = !q.is_empty();
                match q.last_mut() {
                    Some(last) => last.push(task.clone()),
                    None => q.push(vec![task.clone()]),
                }
                let last = q.last().cloned().unwrap_or_default();
                let before = if last.len() > 1 { merged(&last[..last.len() - 1]) } else { task.clone() };
                let spawn = !was_busy;
                if spawn {
                    i.busy.insert(wk.clone(), Busy::default());
                }
                i.save();
                let waiting = (was_busy && queued).then_some(before);
                (spawn, waiting, i.history.get(&wk).cloned().unwrap_or_default())
            });
            if spawn {
                self.spawn_worker(ad, key, agent);
            } else if let Some(prior) = waiting {
                let v = self.route(&prior, &recent, &m.text).await;
                tracing::info!("route {wk} {} -> {:?} (waiting)", v.path, v.route);
                if v.hold {
                    self.with(|i| i.busy.get_mut(&wk).map(|b| b.hold = true));
                } else if v.route == Route::Stop {
                    let _ = self.stop(ad, &wk, Some(m)).await;
                }
            }
            self.remember(&wk, m);
            return;
        };
        let recent = self.with(|i| i.history.get(&wk).cloned().unwrap_or_default());
        let v = self.route(&running_task, &recent, &m.text).await;
        tracing::info!("route {wk} {} -> {:?}", v.path, v.route);
        self.remember(&wk, m);
        if v.route == Route::Stop {
            let _ = self.stop(ad, &wk, Some(m)).await;
            return;
        }
        if v.route == Route::Interrupt {
            let status = self.with(|i| {
                let now = i.busy.get_mut(&wk).filter(|b| b.pid == pid && !b.exited)?;
                now.ended = Some(Ended::Interrupted);
                now.redo = true;
                let status = now.status.clone();
                let mut redo = now.batch.clone();
                let more_wanted = std::mem::take(&mut now.cont);
                let q = i.st.inbox.entry(wk.clone()).or_default();
                if more_wanted && !q.is_empty() {
                    redo.extend(q.remove(0)); // extras for this same work that were waiting
                }
                redo.push(task.clone());
                q.insert(0, redo); // the same work again, corrected, before anything else
                if let Some(pid) = pid {
                    kill(pid);
                }
                i.save();
                Some(status)
            });
            if let Some(status) = status {
                ad.react(m, Reaction::Redo).await;
                ad.status_text(&status, "restarting with your correction…").await;
                return;
            }
        }
        let separate = v.route == Route::Separate;
        let (status, queued, spawn) = self.with(|i| {
            let q = i.st.inbox.entry(wk.clone()).or_default();
            let new_batch = separate || q.is_empty();
            if new_batch {
                q.push(vec![task.clone()]);
            } else if let Some(last) = q.last_mut() {
                last.push(task.clone());
            }
            let first = q.len() == 1;
            let n: usize = q.iter().map(Vec::len).sum();
            let status = i.busy.get_mut(&wk).map(|now| {
                if new_batch {
                    now.cont = !separate && first; // the next batch continues the running work
                }
                now.hold = now.hold || v.hold;
                now.status.clone()
            });
            let spawn = status.is_none();
            if spawn {
                i.busy.insert(wk.clone(), Busy::default());
            }
            i.save();
            (status, n, spawn)
        });
        if spawn {
            self.spawn_worker(ad, key, agent); // the run ended while Jev answered
        } else if let Some(status) = status {
            let suffix = if queued > 0 { format!(" · {queued} queued") } else { String::new() };
            ad.status_text(&status, &format!("working…{suffix}")).await;
        }
    }

    pub(super) fn spawn_worker(self: &Arc<Self>, ad: &Arc<dyn Adapter>, key: &str, agent: &str) {
        let (core, ad, key, agent) = (self.clone(), ad.clone(), key.to_owned(), agent.to_owned());
        tokio::spawn(async move { core.worker(ad, key, agent).await });
    }

    /// Drains the thread's inbox for one agent: each batch of merged messages is one turn. The work key's `Busy`
    /// entry exists before this starts.
    async fn worker(self: Arc<Self>, ad: Arc<dyn Adapter>, key: String, agent: String) {
        let wk = format!("{key}#{agent}");
        let wid = short(&wk);
        self.with(|i| i.works.insert(wid.clone(), wk.clone()));
        let typing = {
            let (ad, conv) = (ad.clone(), self.conv_of(&key));
            tokio::spawn(async move { ad.typing(&conv).await })
        };
        let mut claim = Claim { core: self.clone(), wk: wk.clone(), typing, done: false };
        if let Err(e) = self.work(&ad, &key, &agent, &wk, &wid, &mut claim).await {
            tracing::warn!("worker {wk} failed: {e:#}");
        }
    }

    async fn work(
        &self,
        ad: &Arc<dyn Adapter>,
        key: &str,
        agent: &str,
        wk: &str,
        wid: &str,
        claim: &mut Claim,
    ) -> anyhow::Result<()> {
        loop {
            // Leaving and the check for more are one step, so a message can't arrive in between and wait unseen.
            let idle = self.with(|i| {
                let idle = i.st.inbox.get(wk).is_none_or(Vec::is_empty);
                if idle {
                    i.busy.remove(wk);
                    i.st.inbox.remove(wk);
                    i.save();
                }
                idle
            });
            if idle {
                claim.done = true;
                return Ok(());
            }
            tokio::time::sleep(self.settings.debounce).await;
            // The last message looked unfinished: give the rest time.
            while self.with(|i| i.busy.get_mut(wk).is_some_and(|b| std::mem::take(&mut b.hold))) {
                tokio::time::sleep(self.settings.hold).await;
            }
            // The working line shows while the turn waits for room, and its batch stays in the inbox until the gate
            // lets it go: messages that arrive meanwhile still join it, and a correction can't miss it.
            let conv = self.conv_of(key);
            let status = match ad.status(&conv, agent, wid).await {
                Ok(s) => s,
                Err(e) => {
                    // No button is fine: typing "stop" still works.
                    tracing::warn!("status message {wk}: {e:#}");
                    Value::Null
                }
            };
            let cancel = self.with(|i| {
                let b = i.busy.get_mut(wk)?;
                b.status.clone_from(&status);
                (b.waiting, b.ended, b.pid, b.exited, b.task, b.batch) =
                    (true, None, None, false, String::new(), Vec::new());
                b.cancel = Arc::default();
                Some(b.cancel.clone())
            });
            // Someone who isn't the owner waits behind the owner's turns and shares the invitees' slot (`agent::gate`).
            let person = self.with(|i| {
                i.st.threads.get(key).and_then(|t| t.get("person")).and_then(Value::as_str).unwrap_or("").to_owned()
            });
            let invitee = !person.is_empty() && !hq::resolve(&self.paths, &person, agent).await.owner;
            let permit = match cancel {
                Some(cancel) => self.admit(ad, wk, &cancel, invitee).await,
                None => None,
            };
            // Access can end while the turn waited for the gate: look again right before the batch is taken, so a
            // sweep that found nothing stoppable (or ran before this turn existed) can't be outrun. Asked in sync
            // with the Kith cloud (waiting up to `sync_hold`), as a revoke made on another computer may not be here
            // yet. The owner's own runs are never refused here.
            let refused = if invitee { Some(self.access(&person, agent).await).filter(|a| !a.allowed) } else { None };
            if let Some(access) = refused {
                tracing::info!("{wk}: {person} may not use {agent} now ({}); not starting the turn", access.why);
                self.with(|i| {
                    i.st.inbox.insert(wk.to_owned(), Vec::new());
                    i.save();
                });
            }
            let taken = self.with(|i| {
                let b = i.busy.get_mut(wk)?;
                b.waiting = false;
                let q = i.st.inbox.get_mut(wk).filter(|q| !q.is_empty())?; // Stop emptied it while it waited
                let batch = q.remove(0);
                if let Some(notes) = i.st.wakes.get_mut(wk) {
                    notes.retain(|n| !batch.contains(n));
                    if notes.is_empty() {
                        i.st.wakes.remove(wk);
                    }
                }
                let b = i.busy.get_mut(wk)?;
                b.cont = false; // whatever is queued now came in before this turn started
                let redo = std::mem::take(&mut b.redo);
                let task = format!("{}{}", if redo { INTERRUPTED } else { "" }, merged(&batch));
                b.task.clone_from(&task);
                b.batch = batch;
                i.save();
                Some(task)
            });
            let ended = match (permit, taken) {
                (Some(permit), Some(task)) => self.run(ad, key, agent, wk, &task, permit).await?,
                _ => Ended::Stopped,
            };
            self.with(|i| i.busy.get_mut(wk).map(|b| b.status = Value::Null));
            ad.status_end(&conv, agent, &status, ended).await;
        }
    }

    /// Empties the inbox and ends the running turn, or the one about to start.
    pub(super) async fn stop(&self, ad: &Arc<dyn Adapter>, wk: &str, m: Option<&Incoming>) -> anyhow::Result<()> {
        let active = self.with(|i| {
            i.st.inbox.insert(wk.to_owned(), Vec::new());
            i.save();
            let now = i.busy.get_mut(wk)?;
            if now.stoppable() {
                now.ended = Some(Ended::Stopped);
                if let Some(pid) = now.pid {
                    kill(pid); // the worker says "stopped" when the run comes back
                }
                now.cancel.notify_one();
                return Some(());
            }
            None
        });
        if active.is_none() {
            let (key, agent) = wk.rsplit_once('#').unwrap_or((wk, ""));
            let conv = self.with(|i| i.st.threads.get(key).map(|t| t.get("conv").cloned().unwrap_or(Value::Null)));
            if let Some(conv) = conv {
                ad.status_end(&conv, agent, &Value::Null, Ended::Stopped).await;
            }
        }
        if let Some(m) = m {
            ad.react(m, Reaction::Stop).await; // only messages acted on get a reaction; queued ones are counted
        }
        Ok(())
    }

    /// Ends every run of `person`'s threads (their access is gone), or with `only_denied` those on an agent they may
    /// no longer use. Their queued messages go too, so nothing starts the run again. -> how many runs it ended.
    pub async fn stop_person(&self, person: &str, only_denied: bool) -> usize {
        let mine: Vec<String> = self.with(|i| {
            // The inbox is persisted: a batch left from before a restart waits there without a `Busy` and would start at
            // the next resume, so it is swept too.
            i.busy
                .keys()
                .chain(i.st.inbox.keys())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .filter(|wk| {
                    let key = wk.rsplit_once('#').map_or(wk.as_str(), |(k, _)| k);
                    i.st.threads.get(key).and_then(|t| t.get("person")).and_then(Value::as_str) == Some(person)
                })
                .map(|wk| (*wk).clone())
                .collect()
        });
        let mut stopped = 0;
        for wk in mine {
            let agent = wk.rsplit_once('#').map_or("", |(_, a)| a);
            if only_denied && hq::resolve(&self.paths, person, agent).await.allowed {
                continue;
            }
            let ended = self.with(|i| {
                i.st.inbox.insert(wk.clone(), Vec::new());
                let now = i.busy.get_mut(&wk).filter(|b| b.stoppable())?;
                now.ended = Some(Ended::Stopped);
                if let Some(pid) = now.pid {
                    kill(pid);
                }
                now.cancel.notify_one();
                i.save();
                Some(())
            });
            stopped += usize::from(ended.is_some());
        }
        stopped
    }

    /// The Stop button -> what to show on it.
    pub async fn stop_pressed(&self, ad: &Arc<dyn Adapter>, wid: &str, uid: &str) -> StopPress {
        let wk = self.with(|i| i.works.get(wid).cloned());
        let key = wk.as_ref().map(|wk| wk.rsplit_once('#').map_or(wk.as_str(), |(k, _)| k).to_owned());
        let th = key.and_then(|k| self.with(|i| i.st.threads.get(&k).cloned()));
        let w = hq::who(&self.paths, ad.platform(), uid).await;
        let (Some(wk), Some(th)) = (wk, th) else { return StopPress::NotYours };
        if !Self::owns(&th, &w) {
            return StopPress::NotYours;
        }
        let stopped = self.with(|i| {
            let now = i.busy.get_mut(&wk).filter(|b| b.waiting || !(b.pid.is_some() && b.exited))?;
            i.st.inbox.insert(wk.clone(), Vec::new());
            now.ended = Some(Ended::Stopped);
            if let Some(pid) = now.pid {
                kill(pid);
            }
            now.cancel.notify_one();
            i.save();
            Some(())
        });
        if stopped.is_some() {
            tracing::info!("stop {wk} pressed by {uid}");
            StopPress::Stopping
        } else {
            StopPress::Finished
        }
    }

    /// An approved card's resume for the thread behind work key `wk`: `text` joins the thread's inbox as a batch of
    /// its own and a worker starts if none runs. The card store is the record of the owed resume, not this inbox
    /// (the inbox is emptied before a run starts): `claim` is the card's compare-and-set, taken only once the thread
    /// and its connection are known to be here, so a refusal leaves the resume owed. It returns the ids of the cards it
    /// took (none: someone else did). If the turn that carries `text` then fails to start (the session is held, the
    /// spawn failed), [`Self::wake_not_run`] gives the cards back to the store, so the resume stays owed (C5).
    pub fn wake(self: &Arc<Self>, wk: &str, text: &str, claim: impl FnOnce() -> Vec<String>) -> Wake {
        let (key, agent) = wk.rsplit_once('#').unwrap_or((wk, ""));
        let ad =
            self.ads.locked().iter().find(|(conn, _)| key.starts_with(&format!("{conn}:"))).map(|(_, ad)| ad.clone());
        let Some(ad) = ad else { return Wake::Offline };
        let spawn = self.with(|i| {
            if !i.st.threads.contains_key(key) {
                return None;
            }
            let cards = claim();
            if cards.is_empty() {
                return Some(None);
            }
            self.wakes.locked().push((text.to_owned(), cards));
            i.st.inbox.entry(wk.to_owned()).or_default().push(vec![text.to_owned()]);
            i.st.wakes.entry(wk.to_owned()).or_default().push(text.to_owned());
            let spawn = !i.busy.contains_key(wk);
            if spawn {
                i.busy.insert(wk.to_owned(), Busy::default());
            }
            i.save();
            Some(Some(spawn))
        });
        match spawn {
            None => Wake::NoThread,
            Some(None) => Wake::Claimed,
            Some(Some(spawn)) => {
                if spawn {
                    self.spawn_worker(&ad, key, agent);
                }
                Wake::Queued
            }
        }
    }

    /// The process of the turn that carries `task` is up: its wakes are delivered (M5). Their record stays until the
    /// turn is over, as a run the kernel then refuses (a held session) gives them back.
    pub(super) fn wake_started(&self, task: &str) {
        let ids: Vec<String> = self
            .wakes
            .locked()
            .iter()
            .filter(|(note, _)| task.contains(note.as_str()))
            .flat_map(|(_, cards)| cards.clone())
            .collect();
        if !ids.is_empty()
            && let Some(hub) = crate::runauth::hub()
        {
            crate::cards::resume::channel_started(&hub, &ids);
        }
    }

    /// The turn that carries `task` didn't run: its wakes go back to the store as owed. `again`: offered again now;
    /// after a Stop they wait for the next offer.
    pub(super) fn wake_not_run(&self, task: &str, again: bool) {
        let back = self.settle_wake(task, false);
        if !back.is_empty()
            && let Some(hub) = crate::runauth::hub()
        {
            crate::cards::resume::channel_failed(&hub, &back, again);
        }
    }

    /// The turn that carried `task` is over. `started`: it produced a result, so the wake was delivered. Otherwise (no
    /// result: the kernel refused the held session, or the spawn failed) the cards go back to the store as owed.
    pub(super) fn settle_wake(&self, task: &str, started: bool) -> Vec<String> {
        let done: Vec<Vec<String>> = {
            let mut wakes = self.wakes.locked();
            let (mine, rest): (Vec<_>, Vec<_>) = wakes.drain(..).partition(|(note, _)| task.contains(note.as_str()));
            *wakes = rest;
            mine.into_iter().map(|(_, cards)| cards).collect()
        };
        if started { Vec::new() } else { done.into_iter().flatten().collect() }
    }

    /// Messages that waited for this connection when the host last stopped.
    pub fn resume(self: &Arc<Self>, ad: &Arc<dyn Adapter>) {
        self.ads.locked().insert(ad.conn().to_owned(), ad.clone());
        let prefix = format!("{}:", ad.conn());
        let waiting: Vec<(String, String)> = self.with(|i| {
            let mut out = Vec::new();
            let works: Vec<String> =
                i.st.inbox.iter().filter(|(_, b)| !b.is_empty()).map(|(wk, _)| wk.clone()).collect();
            for wk in works {
                let (key, agent) = wk.rsplit_once('#').unwrap_or((&wk, ""));
                if key.starts_with(&prefix) && !i.busy.contains_key(&wk) && i.st.threads.contains_key(key) {
                    out.push((key.to_owned(), agent.to_owned()));
                    i.busy.insert(wk.clone(), Busy::default());
                }
            }
            out
        });
        for (key, agent) in waiting {
            self.spawn_worker(ad, &key, &agent);
        }
    }
}
