//! M4-2 (the schedule store): the owner's view of the schedules, from the agents' `jobs.json` definitions joined with the
//! run state in the core's database (tables in `core/runtime/src/migrations/schedules/`). Experimental and owner only.
//!
//! The reads are `Served::Core` so a test core answers them. The fire rows (`schedule.tick`, `.runNow`, `.fire`, M4-3) are
//! served too, but nothing runs them on a clock yet: the host and `kith schedule` keep `kith-sched` until the M4-5 switch.
//! `schedule.jobs` is the store's listing: `schedule.list` is the existing
//! `kith schedule ls --json` row (kernel, in `json_forms_more`), which stays as it is until the switch.
//!
//! The write rows (`schedule.add`, `.remove`, `.pause`, `.resume`, `.run`) exist in `json_forms_more` as the kernel's
//! `kith schedule --json` forms and stay `kith-sched`'s own while the schedules are not switched. Once they are, `kith-sched`
//! hands those verbs to the core through the rows below (`schedule.put`, `.drop`, `.setPaused`, `.runNow`, `.runs`, `.jobs`),
//! which have no command line of their own: the old rows describe the CLI's shape, these are what it calls. (`kith-sched`
//! has no edit verb, so there is no `schedule.edit`.)

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
    act(
        "schedule.tick",
        "Fire what is due at `now` (the caller's clock, ms; this machine's by default): one transaction per job that writes its fire row, its turn and the watermark. Catch-up fires only the latest due slot inside a day; older ones are kept as one `skipped:missed` row. Nothing runs this on a clock yet: `kith-sched` owns jobs.json until M4-5.",
        &[flag(
            "now",
            "int",
            &["--now"],
            "The clock, ms since the epoch (this machine's by default).",
        )],
        &["schedule", "tick"],
    ),
    act(
        "schedule.runNow",
        "Run a job now through the same fire: it obeys the skip rule. A busy job answers `busy` and writes nothing, so a retry fires once it is free; a `key` that was decided answers what was decided; a paused or revoked job answers final.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("id", "string", true, "The job."),
            flag(
                "key",
                "string",
                &["--key"],
                "The run's own key (letters, digits, - _ .); a retry with the same key is the same run.",
            ),
            flag(
                "now",
                "int",
                &["--now"],
                "The clock, ms since the epoch (this machine's by default).",
            ),
        ],
        &["schedule", "run-now"],
    ),
    // The verbs of `kith schedule` once the schedules are the core's (`kith kernel core-call`; no command line of their own, the
    // CLI formats the answers as `kith-sched` did). They edit the agent's jobs.json under its `.jobs.lock` and bring the index up
    // to it in the same call, so a pause or a removal cancels the job's waiting turns at once.
    read(
        "schedule.runs",
        "The newest run of every job that has one (of one agent, or one job): what `kith schedule log` lists. Answers `{runs}`.",
        &[
            pos("agent", "string", false, "Only this agent's."),
            pos("id", "string", false, "Only this job's."),
        ],
        &[],
    )
    .no_cli(),
    act(
        "schedule.put",
        "Add a job to an agent's jobs.json, or replace the one with that id (`kith schedule add`). The cron is checked as the fire reads it; `effort` is one of low, medium, high, xhigh, max; with no `id` it is a slug of the task. `person` is who the job runs as (the caller's person; none is the owner). Answers `{ok, agent, replaced, job}`, the job as `schedule.jobs` shows it.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("cron", "string", true, "Five cron fields, or @hourly, @daily, @weekly, @monthly."),
            pos("task", "string", true, "What the run is asked to do."),
            pos("id", "string", false, "The job's id."),
            pos("model", "string", false, "The model."),
            pos("effort", "string", false, "The effort."),
            pos("harness", "string", false, "The engine."),
            pos("person", "string", false, "Who the job runs as."),
        ],
        &[],
    )
    .no_cli(),
    act(
        "schedule.drop",
        "Remove a job from an agent's jobs.json (`kith schedule rm`); what it has waiting is cancelled. Answers `{ok, agent, id, changed}`.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("id", "string", true, "The job."),
        ],
        &[],
    )
    .no_cli(),
    act(
        "schedule.setPaused",
        "Pause or resume a job (`kith schedule pause` and `resume`); pausing cancels what it has waiting. Answers `{ok, agent, id, changed}`.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("id", "string", true, "The job."),
            pos("paused", "bool", true, "True to pause, false to resume."),
        ],
        &[],
    )
    .no_cli(),
    // The host takes a webhook or event delivery at its listener and hands it over whole: the core checks the proof, the
    // event and the job, and fires. Not switched: the host's own routines keep deciding until M4-5.
    act(
        "schedule.fire",
        "A webhook or event delivery for one job: checks the job's key (`Authorization: Bearer` or a GitHub-style `X-Hub-Signature-256` HMAC of the exact body, the key read from the secrets lane), the event against the job's trigger and filters, then fires through the same transaction as `schedule.runNow` with the slot `event:<hash of the delivery id>`, so a redelivery fires at most once. Answers `fired`, `busy` (retry), `ignored`, `ping`, `refused` (`unauthorized`, `paused`, `gone`, `invalid`) or what an earlier delivery of the same id decided. The body is UTF-8 text: the listener refuses a body that is not as `invalid` and never converts it lossily (a replaced byte would make a correct signature fail). The host calls it as itself with the real sender in `caller`; `caller` is only echoed in the answer, never used to decide anything, and M4-5 stores it with the fire row.",
        &[
            pos("agent", "string", true, "The agent."),
            pos("id", "string", true, "The job."),
            pos("headers", "{string}", true, "The request headers, name to value."),
            pos("body", "string", true, "The exact request body (text)."),
            pos("caller", "string", true, "Who sent it, as the host knows (the real principal). Echoed, never used to decide."),
        ],
        &["schedule", "fire"],
    )
    .principals(HOST)
    .no_cli(),
];

const fn act(
    name: &'static str,
    summary: &'static str,
    params: &'static [Param],
    path: &'static [&'static str],
) -> Method {
    read(name, summary, params, path)
}

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
