//! Claude Code's command line for a plan: what `kernel/bin/kith` puts in `args` before the task, for the launch object and
//! for a run in this terminal.

use super::assemble::pj_indent1;
use super::plan::Plan;

/// The arguments of a Claude run, the settings and the MCP config inline. The owner at their own terminal gets Claude Code
/// whole (its plugins, `/mcp`, their skills and MCP servers, the agent's granted servers on top); only runs between agents, the
/// host's launches and everyone else's runs go lean.
pub fn claude_args(plan: &Plan) -> Vec<String> {
    build(plan, None)
}

/// Where a run here keeps what a launch hands over inline: the settings and the owner's MCP config are files in the run
/// folder (a guest's grants are added to the settings file after the claim). A guest's MCP config stays inline.
pub struct Files<'a> {
    pub settings: &'a str,
    pub mcp: &'a str,
}

/// The arguments of a run here, up to the task: the same as [`claude_args`] with the files, then what the caller asked of the
/// run (`KITH_OUTPUT`, the session it continues).
pub fn run_args(plan: &Plan, files: &Files<'_>) -> Vec<String> {
    build(plan, Some(files))
}

fn build(plan: &Plan, files: Option<&Files<'_>>) -> Vec<String> {
    let mut v: Vec<String> = vec!["--append-system-prompt".into(), plan.system_prompt.clone()];
    for d in &plan.out.add_dirs {
        v.extend(["--add-dir".into(), d.clone()]);
    }
    v.extend([
        "--settings".into(),
        files.map_or_else(
            || plan.settings.as_ref().map(pj_indent1).unwrap_or_default(),
            |f| f.settings.to_owned(),
        ),
    ]);
    v.push("--allowedTools".into());
    v.extend(plan.out.allowed_tools.iter().cloned());
    if !plan.owner_terminal {
        v.push("--strict-mcp-config".into());
    }
    v.extend([
        "--mcp-config".into(),
        match files {
            Some(f) if plan.owner => f.mcp.to_owned(),
            _ => plan.mcp_arg.clone(),
        },
    ]);
    let own_mcp = plan.pkg.join("mcp.json");
    if !plan.launch && plan.owner && own_mcp.is_file() {
        v.extend([
            "--mcp-config".into(),
            own_mcp.to_string_lossy().into_owned(),
        ]);
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
    if files.is_some() && plan.out_format == "json" {
        v.extend(["--output-format".into(), "json".into()]);
    }
    if !plan.out.mode_arg.is_empty() {
        v.extend(["--permission-mode".into(), plan.out.mode_arg.clone()]);
    }
    // A session another engine made ("codex:...") can't be resumed here: a new one starts.
    if files.is_some() && !plan.resume.is_empty() {
        if plan.resume.contains(':') {
            eprintln!(
                "kith: session {} is not a Claude Code session; starting a new one",
                plan.resume
            );
        } else {
            v.extend(["--resume".into(), plan.resume.clone()]);
        }
    }
    v
}
