//! M2 lane E (cards): the CLI path for cards, so one raised while no app runs can be answered from a terminal
//! (runtime-design section 5). Experimental stubs, owner principal only, served by nobody in the core yet.

use super::build::*;

pub(super) const ROWS: &[Method] = &[
    card(
        "cards.list",
        "The cards waiting, and recent ones.",
        &[],
        &["cards", "ls"],
    ),
    card(
        "cards.show",
        "One card.",
        &[pos("id", "string", true, "The card.")],
        &["cards", "show"],
    ),
    card(
        "cards.answer",
        "Answer a card.",
        &[
            pos("id", "string", true, "The card."),
            pos(
                "answer",
                "string",
                true,
                "The answer (the button's value, or the text asked for).",
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
        Served::Kernel,
        Stability::Experimental,
    )
    .formats(J)
}
