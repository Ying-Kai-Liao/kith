//! Where places work: hq projects, recent and browsable folders, and the folder of a place (see `agent::folder`).
//! The contract the apps build their pickers on is in `serve/docs/place-folders.md`.

use super::str_arg;
use crate::agent::folder::{self, Change};
use crate::agent::hq;
use crate::hub::Hub;
use crate::store::Lane;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;

pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        // hq's registry, never an error status: a computer without hq has no projects, and says why.
        "projects" => match hq::projects(b["refresh"] == true).await {
            Ok(list) => json!({"projects": list.iter().map(folder::project_value).collect::<Vec<_>>()}),
            Err(e) => json!({"projects": [], "error": format!("{e:#}")}),
        },
        "recentFolders" => {
            let hub = hub.clone();
            json!({"folders": tokio::task::spawn_blocking(move || folder::recent_folders(&hub.store)).await?})
        }
        "listDirs" => {
            // Marking projects is a courtesy: without hq the folders list as plain ones.
            let projects = hq::projects_soon().await;
            let path = b["path"].as_str().map(str::to_owned);
            let hidden = b["showHidden"] == true;
            tokio::task::spawn_blocking(move || folder::list_dirs(path.as_deref(), hidden, &projects)).await??
        }
        "setPlaceFolder" => {
            let bot = str_arg(&b, "botId")?;
            let placed = hub.set_place_folder(bot, &lane_arg(&b, bot), Change::from_args(&b)?).await?;
            json!({"folder": placed})
        }
        "registerProject" => {
            let (id, path) = (str_arg(&b, "id")?.trim().to_owned(), str_arg(&b, "path")?.to_owned());
            if !folder::valid_project_id(&id) {
                bail!("`{id}` isn't a project id (lowercase words joined by dashes)");
            }
            let canon = tokio::task::spawn_blocking(move || folder::check(Path::new(&path))).await??;
            let path = canon.to_string_lossy().into_owned();
            hq::project_add(&id, &path).await?;
            // Registered; if the list can't be read back, say what was registered.
            let listed = hq::projects(true).await.ok().and_then(|l| l.into_iter().find(|p| p.id == id));
            let project = listed.unwrap_or(hq::Project { id, paths: vec![path], ..hq::Project::default() });
            json!({"project": folder::project_value(&project)})
        }
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

/// The place a call names: the bot's own chat, its thread (`threadId`, the root message), or its lane in a group
/// (`chat`, the group's id).
pub(super) fn lane_arg(b: &Value, bot: &str) -> Lane {
    let chat = b["chat"].as_str().filter(|c| !c.is_empty()).unwrap_or(bot);
    match b["threadId"].as_str().filter(|t| !t.is_empty()) {
        Some(root) => Lane::in_thread(chat, root),
        None => Lane::main(chat),
    }
}
