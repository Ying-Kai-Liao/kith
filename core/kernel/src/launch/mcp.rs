//! The MCP servers of a launch (`kith AGENT --launch-json`'s `mcp_servers`): the granted shared services plus the agent's
//! own `mcp.json`, as one `{"mcpServers": {...}}`; anyone but the owner gets `kith-permit` and nothing else.
//!
//! The granted part is `kith-auth mcp-config AGENT`, which resolves each service's secrets (they move in A5): this module
//! takes that command's output as an argument and ports only the merge.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};

/// `permit_mcp`: a guest's one server, the way out of the sandbox as a request.
pub fn permit(kernel: &Path) -> Value {
    json!({"mcpServers": {"kithpermit": {"command": format!("{}/bin/kith-permit", kernel.display()), "args": []}}})
}

/// `merged_mcp`: `granted` is the output of `kith-auth mcp-config` (`None`: it failed, no shared services this run);
/// `own` is the agent's `mcp.json`, read from `pkg` when it is there. Later files win a server of the same name.
pub fn merged(
    owner: bool,
    kernel: &Path,
    granted: Option<&Value>,
    pkg: &Path,
) -> Result<Value, String> {
    if !owner {
        return Ok(permit(kernel));
    }
    let own_file = pkg.join("mcp.json");
    let own = if own_file.is_file() {
        let text =
            fs::read_to_string(&own_file).map_err(|e| format!("{}: {e}", own_file.display()))?;
        Some(
            serde_json::from_str::<Value>(&text)
                .map_err(|e| format!("{}: {e}", own_file.display()))?,
        )
    } else {
        None
    };
    let empty = json!({"mcpServers": {}});
    let mut servers = Map::new();
    for d in [granted.unwrap_or(&empty)].into_iter().chain(own.as_ref()) {
        let Value::Object(o) = d else {
            return Err("an MCP config is not a JSON object".to_owned());
        };
        // a file without "mcpServers" is the servers themselves
        match o.get("mcpServers") {
            Some(Value::Object(s)) => servers.extend(s.clone()),
            Some(_) => return Err("mcpServers is not an object".to_owned()),
            None => servers.extend(o.clone()),
        }
    }
    Ok(json!({"mcpServers": servers}))
}
