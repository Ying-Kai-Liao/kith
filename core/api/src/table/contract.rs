//! Family 2: the verbs the host reads from the kernel (`serve/contract/kernel/contract.json`), typed from the contract.

use super::build::*;
use crate::Method;

pub(super) const ROWS: &[Method] = &[
    contract("agents.list", "The agents on this Mac.", &[], "[Agent]", &["ls"]).requires(J),
    contract("projects.list", "The registered projects.", &[], "Projects", &["project", "ls"]).requires(J),
    contract(
        "agents.launch",
        "How to start an agent (run in the folder the run works in; `KITH_PERSON` makes it an invited person's run).",
        &[
            pos("agent", "string", true, "The agent."),
            flag("backup", "string", &["--backup"], "Launch a backup chain entry instead of the agent's own login."),
            flag("project", "string", &["-p"], "Load that project's context."),
        ],
        "Launch",
        &[],
    )
    .requires(&["--launch-json"]),
    contract(
        "agents.create",
        "Make an agent.",
        &[
            pos("name", "string", true, "Lowercase letters, digits, dashes."),
            pos("description", "string", true, "One line, no ': '."),
            flag("instructions", "string", &["--instructions"], "A file holding the first draft of agent.md's opening paragraph."),
        ],
        "NewAgent",
        &["new"],
    )
    .requires(J),
    contract("delegations.list", "The delegation records on this Mac.", &[], "Delegations", &["ask", "delegations"]).requires(J),
];
