//! M2 lane B (secrets in the envelope): the owner's door to it. The host vault is a client of these rows over the owner
//! socket (M2-7), because the host may not link the core's internals (runtime-design section 9b rule 6). Values cross
//! as text and only on the owner socket (`get` only on the host's connection); `put` takes its value as a caller param, never on a command line. Experimental
//! until kith-auth and rollback (M2-18) have used them; not in `released.json`.

use super::build::*;

pub(super) const ROWS: &[Method] = &[
    row(
        "secrets.open",
        "Load the machine's secrets key from the OS key store (making it on a first use), so later calls do not wait for the store.",
        &[],
        "json",
        &["secrets", "open"],
        Served::Core,
        Stability::Experimental,
    )
    .formats(J),
    row(
        "secrets.has",
        "Whether a secret of that name is stored. Needs no key, so it never waits on the OS key store.",
        &[pos("name", "string", true, "The secret's name.")],
        "json",
        &["secrets", "has"],
        Served::Core,
        Stability::Experimental,
    )
    .formats(J),
    row(
        "secrets.delete",
        "Remove a secret. Needs no key.",
        &[pos("name", "string", true, "The secret's name.")],
        "json",
        &["secrets", "delete"],
        Served::Core,
        Stability::Experimental,
    )
    .formats(J),
    row(
        "secrets.get",
        "A secret's value, or that there is none.",
        &[pos("name", "string", true, "The secret's name.")],
        "json",
        &["secrets", "get"],
        Served::Core,
        Stability::Experimental,
    )
    .formats(J)
    // The only row that hands a value back: the host (the vault) proves its in-memory secret for it. The owner token
    // is a file any same-user process can read, so it gets has/put/delete/open and never the value (M2-10c).
    .principals(HOST)
    .no_cli(),
    row(
        "secrets.put",
        "Store a secret sealed under the machine's key, replacing an earlier value of that name.",
        &[
            pos("name", "string", true, "The secret's name."),
            caller(
                "value",
                "string",
                "The value, from the caller (never a word of the command line).",
            ),
        ],
        "json",
        &["secrets", "put"],
        Served::Core,
        Stability::Experimental,
    )
    .formats(J),
    row(
        "secrets.launchEnv",
        "The keys one agent is granted, for a launch: for each granted service, its profile and the values of that profile's variables. Only that agent's grants, never another agent's keys.",
        &[pos("agent", "string", true, "The agent being launched.")],
        "json",
        &["secrets", "launch-env"],
        Served::Core,
        Stability::Experimental,
    )
    .formats(J)
    // Owner only, and it hands values back: the narrow door kith-auth uses at the switch (M2-18b, q223), so `get` can
    // stay the host's alone. Not wired into kith-auth yet.
    .no_cli(),
];
