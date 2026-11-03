//! M2 lane E (cards): the CLI path for cards, so one raised while no app runs can be answered from a terminal
//! (runtime-design section 5). Experimental and owner only, served by the daemon (`Core::call` itself answers NotServed:
//! raising, claiming and closing a card change state, so the daemon runs them on `&mut Core`). The host's own card
//! store still decides until lane H switches its callers; secret cards (a key or access) are answered in the app.

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
