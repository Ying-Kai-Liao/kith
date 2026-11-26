//! A process the host started in a job it holds (Windows, W12-7, rule 6): the resident stub. The host keeps the job's only
//! handle (`kith_winsys::job`, unnamed, kill on close, no breakaway flags), so when the host ends, however it ends, the stub
//! ends with it and the stub's own job ends its engine.

use kith_winsys::job::{Child, Job};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) struct Held {
    child: Child,
    job: Arc<Job>,
}

impl Held {
    pub(crate) fn new(child: Child, job: Arc<Job>) -> Self {
        Self { child, job }
    }

    /// Gives the process `grace` to end by itself (its stdin is closed already), then ends everything in the job. Dropping the
    /// last handle of the job does the same; this makes it happen now instead of when the connection is dropped.
    pub(crate) async fn reap(self, grace: Duration) {
        let by = Instant::now() + grace;
        while matches!(self.child.wait_for(Duration::ZERO), Ok(None)) && Instant::now() < by {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let _ = self.job.terminate(1);
    }
}
