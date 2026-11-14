//! The run token of a room person (rooms with people, PR C): the token of the turns of a group room that has people in it.
//!
//! Its audience is the room `room`, cut to what every person of the turn's basis may see: the token carries the basis
//! ([`RunIdentity::members`]), `kith recall` for it runs with that as `KITH_ROOM_MEMBERS` and the room as `KITH_ROOM`
//! (`run_kernel`), and the kernel's room-person branch (`kith_recall.py` `_caller`) reads exactly those. Nothing of a
//! request, and nothing of the process environment, can change it: never another room, never the owner's notes.
//!
//! Always host-minted. The core mints for the people it knows and a room person is none of them, so with run tokens via the
//! core a room turn has no token and no recall (it fails closed). M3 has to teach the core about basis tokens.

use super::{RunGrant, RunIdentity, SOCKET, TokenScope, session_of, table};

/// A token for `person` (a room person's id) running `agent` in `room` for `basis`, with the environment to give the run.
/// `None` with no basis (a room person is always for somebody), no socket listening, or a table that refuses.
pub fn grant_for_room(
    person: &str,
    agent: &str,
    room: &str,
    basis: &[String],
    session: &str,
    project: Option<&str>,
) -> Option<(RunGrant, [(String, String); 2])> {
    if basis.is_empty() || !crate::chat::room_turn::is_room_person(person) {
        return None;
    }
    let socket = SOCKET.get()?;
    let identity = RunIdentity {
        person: person.to_owned(),
        agent: agent.to_owned(),
        room: Some(room.to_owned()),
        run_id: uuid::Uuid::new_v4().simple().to_string(),
        scope: TokenScope::Guest,
        session: session_of(person, session),
        project: project.filter(|p| !p.is_empty()).map(str::to_owned),
        reply: None,
        members: basis.to_vec(),
    };
    match RunGrant::issue(table(), identity) {
        Ok(grant) => {
            let env = grant.env(socket);
            Some((grant, env))
        }
        Err(error) => {
            tracing::warn!(error = format!("{error:#}"), "no run token issued for a room person");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runauth::{Authorized, PeerInfo, authorize};
    use serde_json::json;

    fn line(token: &str, method: &str, args: &[&str]) -> Vec<u8> {
        json!({"token": token, "method": method, "args": args}).to_string().into_bytes()
    }

    #[test]
    fn a_room_persons_token_holds_its_room_and_basis_and_a_request_cannot_change_them() {
        let t = std::sync::Arc::new(crate::runauth::TokenTable::new());
        let identity = RunIdentity {
            person: "room-grp1-9".into(),
            agent: "echo".into(),
            room: Some("grp1".into()),
            run_id: "r1".into(),
            scope: TokenScope::Guest,
            session: session_of("room-grp1-9", "acp:grp1:"),
            project: None,
            reply: None,
            members: vec!["ann".into(), "ben".into()],
        };
        let grant = RunGrant::issue(&t, identity).unwrap();
        t.bind(&grant.key, 4242);
        let me = PeerInfo { pid: Some(4242) };
        // The request names another room and another person: they are only narrowing words or refused flags.
        let asked = line(&grant.token, "recall", &["zebra", "--room", "1:1:echo:ann", "--json"]);
        let Ok(Authorized { identity, args, .. }) = authorize(&t, &asked, me) else { panic!("refused") };
        assert_eq!((identity.person.as_str(), identity.room.as_deref()), ("room-grp1-9", Some("grp1")));
        assert_eq!(identity.members, ["ann", "ben"], "the basis is the token's");
        assert!(args.iter().all(|a| !a.contains("KITH_")), "{args:?}");
        let sneaky = line(&grant.token, "recall", &["zebra", "--person", "ann"]);
        assert!(authorize(&t, &sneaky, me).is_err(), "who asks is never an argument");
    }

    #[test]
    fn only_a_room_person_with_a_basis_gets_a_room_token() {
        assert!(grant_for_room("sam", "echo", "grp1", &["ann".into()], "s", None).is_none(), "not a room person");
        assert!(grant_for_room("room-grp1-9", "echo", "grp1", &[], "s", None).is_none(), "nobody to be for");
    }
}
