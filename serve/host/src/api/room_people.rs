//! The owner's methods for the people in a group room (rooms with people, `chat/room_people.rs`). Owner only: on
//! neither the member nor the bot list, so an invited person or a bot guest can't add or take out anyone, themselves
//! included.

use super::str_arg;
use crate::chat::room_people;
use crate::hub::Hub;
use anyhow::Result;
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::sync::Arc;

pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "addRoomPerson" => {
            let change = room_people::add_person(hub, str_arg(&b, "room")?, str_arg(&b, "person")?).await?;
            json!({"gen": change.room_gen, "changed": change.changed})
        }
        "removeRoomPerson" => {
            let change = room_people::remove_person(hub, str_arg(&b, "room")?, str_arg(&b, "person")?)?;
            json!({"gen": change.room_gen, "changed": change.changed})
        }
        "roomPeople" => {
            let (room_gen, stays) = room_people::stays(&hub.store, str_arg(&b, "room")?);
            let people: Vec<Value> = stays
                .iter()
                .map(|s| json!({"person": s.person, "joinedGen": s.joined_gen, "leftGen": s.left_gen}))
                .collect();
            json!({"enabled": room_people::enabled(&hub.store), "gen": room_gen, "people": people})
        }
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}
