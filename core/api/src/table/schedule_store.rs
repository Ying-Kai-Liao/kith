//! M4-2 (the schedule store): the owner's view of the schedules, from the agents' `jobs.json` definitions joined with the
//! run state in core.db (tables in `core/runtime/src/migrations/schedules/`). Experimental and owner only.
//!
//! The reads are `Served::Core` so a test core answers them; the daemon runs nothing for them yet (the host and `kith
//! schedule` keep `kith-sched` until M4-5). `schedule.jobs` is the store's listing: `schedule.list` is the existing
//! `kith schedule ls --json` row (kernel, in `json_forms_more`), which stays as it is until the switch.
//!
//! The write rows (`schedule.add`, `.remove`, `.pause`, `.resume`, `.run`) already exist in `json_forms_more` as kernel
//! rows. The one the store adds is `schedule.edit`, kernel-served too: nothing in the core writes a jobs.json, which
//! `kith-sched` keeps to itself until M4-5.

use super::build::*;

pub(super) const ROWS: &[Method] = &[
    read(
        "schedule.jobs",
        "Every job of every agent with its next run and its last run, from jobs.json and the core's run state. Also says which agents' files could not be read, the runs of jobs since removed, and the last tick.",
        &[pos("agent", "string", false, "Only this agent's.")],
        &["schedule", "jobs"],
    ),
    read(
        "schedule.show",
        "One job in full: its definition, next run, last run, recent runs and recent fires. Refuses an agent or job that does not exist.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("id", "string", true, "The job."),
        ],
        &["schedule", "show"],
    ),
    read(
        "schedule.next",
        "The jobs that are not paused, soonest first, with when each runs next.",
        &[pos("agent", "string", false, "Only this agent's.")],
        &["schedule", "next"],
    ),
    row(
        "schedule.edit",
        "Change a job's cron, task, model, effort or harness.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("id", "string", true, "The job."),
            flag("cron", "string", &["--cron"], "The new cron."),
            flag("task", "string", &["--task"], "The new task."),
            flag("model", "string", &["--model", "-m"], "The model."),
            flag("effort", "string", &["--effort", "-e"], "The effort."),
            flag("harness", "string", &["--harness", "-H"], "The harness."),
        ],
        "json",
        &["schedule", "edit"],
        Served::Kernel,
        Stability::Experimental,
    )
    .formats(J),
];

const fn read(
    name: &'static str,
    summary: &'static str,
    params: &'static [Param],
    path: &'static [&'static str],
) -> Method {
    row(
        name,
        summary,
        params,
        "json",
        path,
        Served::Core,
        Stability::Experimental,
    )
    .formats(J)
}
