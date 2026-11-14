//! The two room methods only an invited person has (rooms with people): the rooms they are in, and leaving one. The
//! rest of what they do in a room (history, thread, send, react) is the same methods as in their own chat, read and
//! written by the room's rules (`chat::room_post`). Neither is on the owner's or a bot's list of room methods.

use super::devices::{Caller, NotAllowed, Principal, principal};
use super::str_arg;
use crate::chat::room_post;
use crate::hub::Hub;
use anyhow::Result;
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::sync::Arc;

pub(super) async fn call(hub: &Arc<Hub>, caller: &Caller, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    // Only a person's own device is asked: the guard let it through, and nobody else has these methods.
    let member = || match principal(&hub.store, caller) {
        Principal::Member(person) => Ok(person),
        _ => Err(NotAllowed("This isn't available to this account.".to_owned())),
    };
    Ok(ControlFlow::Break(match method {
        "myRooms" => json!({"rooms": room_post::my_rooms(hub, &member()?).await}),
        "leaveRoom" => json!({"left": room_post::leave(hub, &member()?, str_arg(&b, "room")?)?}),
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}
