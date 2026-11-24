//! Claude Code's command line for a plan: what `kernel/bin/kith` puts in `args` before the task, for the launch object and
//! for a run in this terminal.

use super::assemble::pj_indent1;
use super::plan::Plan;

/// The arguments of a Claude run, the settings and the MCP config inline. The owner at their own terminal gets Claude Code
/// whole (its plugins, `/mcp`, their skills and MCP servers, the agent's granted servers on top); only runs between agents, the
/// host's launches and everyone else's runs go lean.
pub fn claude_args(plan: &Plan) -> Vec<String> {
    let mut v: Vec<String> = vec!["--append-system-prompt".into(), plan.system_prompt.clone()];
    for d in &plan.out.add_dirs {
        v.extend(["--add-dir".into(), d.clone()]);
    }
    v.extend([
        "--settings".into(),
        plan.settings.as_ref().map(pj_indent1).unwrap_or_default(),
    ]);
    v.push("--allowedTools".into());
    v.extend(plan.out.allowed_tools.iter().cloned());
    if !plan.owner_terminal {
        v.push("--strict-mcp-config".into());
    }
    v.extend(["--mcp-config".into(), plan.mcp_arg.clone()]);
    let own_mcp = plan.pkg.join("mcp.json");
    if !plan.launch && plan.owner && own_mcp.is_file() {
        v.extend(["--mcp-config".into(), own_mcp.to_string_lossy().into_owned()]);
    }
    if plan.out.disable_slash_commands && !plan.owner_terminal {
        v.push("--disable-slash-commands".into());
    }
    if !plan.model.is_empty() {
        v.extend(["--model".into(), plan.model.clone()]);
    }
    if !plan.effort.is_empty() {
        v.extend(["--effort".into(), plan.effort.clone()]);
    }
    if !plan.out.mode_arg.is_empty() {
        v.extend(["--permission-mode".into(), plan.out.mode_arg.clone()]);
    }
    v
}
