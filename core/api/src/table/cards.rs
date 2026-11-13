//! M2 lane E (cards): the CLI path for cards, so one raised while no app runs can be answered from a terminal
//! (runtime-design section 5). Experimental and owner only, served by the daemon (`Core::call` itself answers NotServed:
//! raising, claiming and closing a card change state, so the daemon runs them on `&mut Core`). The host's own card
//! store still decides until lane H switches its callers. A key card is answered by `cards.provide` (M2-12): the core seals the
//! key into the envelope and spends the card, once; `cards.answer` never takes a secret card's yes.

use super::build::*;

pub(super) const ROWS: &[Method] = &[
    card(
        "cards.list",
        "The cards waiting (oldest first) and the owner's recent closed ones (newest first), as `{open, recent}`.",
        &[],
        &["cards", "ls"],
    ),
    card(
        "cards.show",
        "One card in full, with its options and who holds its claim. Refuses an id that is no card.",
        &[pos("id", "string", true, "The card.")],
        &["cards", "show"],
    ),
    card(
        "cards.answer",
        "Answers a card. Answers `{result: answered}` (with the card's new state), `closed` (it was already answered, withdrawn or expired), `expired` (its time ran out first) or `claimed` (another answerer holds it). Refuses a secret card (a key or access ask: answer it in the Kith app), an unknown card and an answer the card does not offer.",
        &[
            pos("id", "string", true, "The card."),
            pos(
                "answer",
                "string",
                true,
                "The answer: one of the card's option ids, or allow, deny or cancel when it lists none.",
            ),
        ],
        &["cards", "answer"],
    ),
    card(
        "cards.provide",
        "Types a key into a key card: the core claims the card, seals every value into the secrets envelope and spends the card in one commit, at most once. Answers `{result: saved}` (names only: the service, account and variables, never a value), `closed` (answered, denied, saved or withdrawn already), `expired` (its time ran out first) or `claimed` (another provide holds it). A failed write changes nothing and leaves the card open. Refuses a card that is not a key card, an account name that is taken or unusable, and values that do not match the card. The values are never logged or echoed; a terminal does not type them, an app fills them in.",
        &[
            pos("id", "string", true, "The key card."),
            flag(
                "account",
                "string",
                &["--account"],
                "The account name for a new key (the card names it when it replaces one).",
            ),
            caller(
                "values",
                "{string}",
                "The key's fields by variable name. Never written on a command line.",
            ),
        ],
        &["cards", "provide"],
    ),
];

const fn card(
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
