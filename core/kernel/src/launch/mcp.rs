//! The MCP servers of a launch (`kith AGENT --launch-json`'s `mcp_servers`): the granted shared services plus the agent's
//! own `mcp.json`, as one `{"mcpServers": {...}}`; anyone but the owner gets `kith-permit` and nothing else.
//!
//! The granted part is `kith-auth mcp-config AGENT`, which resolves each service's secrets (they move in A5): this module
//! takes that command's output as an argument and ports only the merge.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};

/// The command that starts the `kithpermit` server: `<program> kernel permit --kernel K` (no verb, so the MCP server). `None`
/// (`permit` not switched, or no program that lists it) names the script at the old path, the Python.
pub fn permit_command(permit_bin: Option<&str>, kernel: &Path) -> (String, Vec<String>) {
    match permit_bin.filter(|b| !b.is_empty()) {
        Some(bin) => (
            bin.to_owned(),
            ["kernel", "permit", "--kernel"]
                .iter()
                .map(|x| (*x).to_owned())
                .chain([kernel.display().to_string()])
                .collect(),
        ),
        None => (format!("{}/bin/kith-permit", kernel.display()), vec![]),
    }
}

/// `permit_mcp`: a guest's one server, the way out of the sandbox as a request.
pub fn permit(permit_bin: Option<&str>, kernel: &Path) -> Value {
    let (command, args) = permit_command(permit_bin, kernel);
    json!({"mcpServers": {"kithpermit": {"command": command, "args": args}}})
}

/// `merged_mcp`: `granted` is the output of `kith-auth mcp-config` (`None`: it failed, no shared services this run);
/// `own` is the agent's `mcp.json`, read from `pkg` when it is there. Later files win a server of the same name.
pub fn merged(
    owner: bool,
    permit_bin: Option<&str>,
    kernel: &Path,
    granted: Option<&Value>,
    pkg: &Path,
) -> Result<Value, String> {
    if !owner {
        return Ok(permit(permit_bin, kernel));
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
