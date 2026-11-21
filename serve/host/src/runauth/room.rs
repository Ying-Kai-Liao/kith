//! The run token of a room person (rooms with people, PR C): the token of the turns of a group room that has people in it.
//!
//! Its audience is the room `room`, cut to what every person of the turn's basis may see: the token carries the basis
//! (`RunIdentity::members` in the core), `kith recall` for it runs with that as `KITH_ROOM_MEMBERS` and the room as `KITH_ROOM`
//! (`runtools` in the core), and the kernel's room-person branch (`kith_recall.py` `_caller`) reads exactly those. Nothing of a
//! request, and nothing of the process environment, can change it: never another room, never the owner's notes.
//!
//! Minted by the core like every guest's token (`run.mint` with `members`): the core checks the room person's id is of this
//! room, that the basis is not empty and that every member of it may still have a run, and a revoked member ends the token.

use super::{RunGrant, grant_via_core};

/// A token for `person` (a room person's id) running `agent` in `room` for `basis`, with the environment to give the run.
/// `None` with no basis (a room person is always for somebody), or when the core refuses or does not answer.
pub fn grant_for_room(
    person: &str,
    agent: &str,
    room: &str,
    basis: &[String],
    session: &str,
    project: Option<&str>,
) -> Option<(RunGrant, Vec<(String, String)>)> {
    if basis.is_empty() || !crate::chat::room_turn::is_room_person(person) {
        return None;
    }
    grant_via_core(person, agent, Some(room), basis, session, project)
}

/// [`grant_for_room`] for async code (it asks the core, which may take a moment): on a blocking thread.
pub async fn grant_for_room_async(
    person: &str,
    agent: &str,
    room: &str,
    basis: &[String],
    session: &str,
    project: Option<&str>,
) -> Option<(RunGrant, Vec<(String, String)>)> {
    let (person, agent, room, basis) = (person.to_owned(), agent.to_owned(), room.to_owned(), basis.to_vec());
    let (session, project) = (session.to_owned(), project.map(str::to_owned));
    tokio::task::spawn_blocking(move || grant_for_room(&person, &agent, &room, &basis, &session, project.as_deref()))
        .await
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_room_person_with_a_basis_gets_a_room_token() {
        assert!(grant_for_room("sam", "echo", "grp1", &["ann".into()], "s", None).is_none(), "not a room person");
        assert!(grant_for_room("room-grp1-9", "echo", "grp1", &[], "s", None).is_none(), "nobody to be for");
    }
}
