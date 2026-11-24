#![allow(clippy::unwrap_used)]

use serde_json::{Value, json};

use super::authority::test_client;
use super::kit::{Fake, Home, NOTE, path_str, svca, tables};
use super::mcp::{Starter, exec_program, mcp_config};
use super::pyjson;

/// A home with `echo` needing `svca`, the profile granted and its token in the environment of the run.
fn home(name: &str) -> (Home, super::session::Env) {
    let h = Home::new(name);
    h.agent("echo", &["svca"]);
    h.service("svca", &svca());
    h.tables(&tables());
    let mut env = h.env();
    env.insert("SVC_TOKEN".to_owned(), "tok".to_owned());
    (h, env)
}

fn config(h: &Home, env: &super::session::Env, starter: &Starter) -> (String, Vec<String>) {
    let made = mcp_config(&h.ctx(), env, "echo", starter).unwrap();
    (pyjson::dumps(&made.config), made.notes)
}

#[test]
fn the_script_form_is_what_the_python_printed() {
    let (h, env) = home("script");
    let k = path_str(&h.kernel());
    let (text, notes) = config(&h, &env, &Starter::Script);
    assert_eq!(notes, Vec::<String>::new());
    assert_eq!(
        text,
        format!(
            "{{\"mcpServers\": {{\"svca\": {{\"command\": \"{k}/bin/kith-auth\", \"args\": [\"exec\", \"--for\", \"echo\", \"svca\", \"--\", \"{k}/bin/kith\", \"svc\", \"echo\", \"{home}/x\"], \"env\": {{\"WHO\": \"agent-echo\"}}}}}}}}",
            home = path_str(&h.0.join("home"))
        )
    );
}

#[test]
fn the_program_form_names_this_program_and_the_verb() {
    let (h, env) = home("program");
    let k = path_str(&h.kernel());
    let (text, _) = config(&h, &env, &Starter::Program("/p/kith".to_owned()));
    let v: Value = serde_json::from_str(&text).unwrap();
    let server = &v["mcpServers"]["svca"];
    assert_eq!(server["command"], "/p/kith");
    let args = server["args"].as_array().unwrap();
    assert_eq!(
        Value::Array(args[..8].to_vec()),
        json!(["kernel", "secrets", "exec", "--for", "echo", "svca", "--", format!("{k}/bin/kith")])
    );
    assert_eq!(server["env"], json!({"WHO": "agent-echo"}));
}

#[test]
fn only_a_program_that_lists_the_token_starts_the_servers() {
    let lists = |_: &std::path::Path| true;
    let lists_not = |_: &std::path::Path| false;
    assert_eq!(
        exec_program(Some("/p/kith"), None, lists),
        Starter::Program("/p/kith".to_owned())
    );
    assert_eq!(
        exec_program(Some("/p/kith"), None, lists_not),
        Starter::Script
    );
    assert_eq!(
        exec_program(Some("/p/kith"), Some("python"), lists),
        Starter::Script
    );
    assert_eq!(
        exec_program(Some("/p/kith"), Some("rust"), lists),
        Starter::Program("/p/kith".to_owned())
    );
    assert_eq!(exec_program(Some(""), None, lists), Starter::Script);
    assert_eq!(exec_program(None, None, lists), Starter::Script);
}

#[test]
fn a_server_is_left_out_with_a_line_and_the_exit_stays_ok() {
    let (h, mut env) = home("left-out");
    // no grant
    let mut t = tables();
    t["grants"] = json!({});
    h.tables(&t);
    let (text, notes) = config(&h, &env, &Starter::Script);
    assert_eq!(text, "{\"mcpServers\": {}}");
    assert_eq!(
        notes,
        [
            "kith: echo needs svca but has no grant; running without it (kith-auth grant echo svca <profile>)"
        ]
    );
    // a secret that does not resolve
    h.tables(&tables());
    env.remove("SVC_TOKEN");
    let (text, notes) = config(&h, &env, &Starter::Script);
    assert_eq!(text, "{\"mcpServers\": {}}");
    assert_eq!(
        notes,
        [
            "kith-auth: p1: TOKEN does not resolve (env:SVC_TOKEN; kith-auth set p1 TOKEN)",
            "kith: echo: svca skipped this run, its secrets don't resolve (kith-auth ls)"
        ]
    );
}

#[test]
fn a_declared_service_with_no_file_is_a_refusal_a_granted_one_is_skipped() {
    let (h, env) = home("no-file");
    std::fs::remove_file(h.kernel().join("services/svca.json")).unwrap();
    let d = mcp_config(&h.ctx(), &env, "echo", &Starter::Script).unwrap_err();
    assert_eq!(
        (d.code, d.msg.as_str()),
        (
            1,
            "no service 'svca' (kernel/services/, or an agent's services/)"
        )
    );
    // not declared (no `needs`), only granted to the agent's own row: skipped with a note
    h.agent("echo", &[]);
    let made = mcp_config(&h.ctx(), &env, "echo", &Starter::Script).unwrap();
    assert_eq!(made.config, json!({"mcpServers": {}}));
    assert_eq!(
        made.notes,
        [
            "kith: echo: svca is granted but its service file is gone; skipped (kith echo connectors)"
        ]
    );
}

#[test]
fn a_granted_but_not_needed_service_reaches_the_owner_only() {
    let (h, env) = home("connected");
    h.agent("echo", &[]);
    let (text, _) = config(&h, &env, &Starter::Script);
    assert!(text.contains("\"svca\""), "{text}");
    // anything that is no mcp server is left out without a word
    h.service("svca", &json!({"kind": "cli", "secrets": []}));
    let (text, notes) = config(&h, &env, &Starter::Script);
    assert_eq!((text.as_str(), notes.len()), ("{\"mcpServers\": {}}", 0));
    // a guest's run gets what the package needs, not what the owner connected
    h.service("svca", &svca());
    let mut guest = env.clone();
    guest.insert("KITH_PERSON".to_owned(), "sam".to_owned());
    let mut c = h.ctx();
    c.person = Some("sam".to_owned());
    let made = mcp_config(&c, &guest, "echo", &Starter::Script).unwrap();
    assert_eq!(made.config, json!({"mcpServers": {}}));
}

#[test]
fn the_person_is_copied_into_the_server_s_env() {
    let (h, mut env) = home("person");
    h.agent("echo", &["svca"]);
    // sam has their own grant of their own profile
    let mut t = tables();
    t["profiles"]["p2"] = json!({"TOKEN": "env:SVC_TOKEN", "person": "sam"});
    t["grants"]["echo@sam"] = json!({"svca": "p2"});
    h.tables(&t);
    env.insert("KITH_PERSON".to_owned(), "sam".to_owned());
    let mut c = h.ctx();
    c.person = Some("sam".to_owned());
    let made = mcp_config(&c, &env, "echo", &Starter::Script).unwrap();
    assert_eq!(
        made.config["mcpServers"]["svca"]["env"],
        json!({"WHO": "agent-echo", "KITH_PERSON": "sam"})
    );
}

#[test]
fn someone_else_s_profile_is_left_out_with_why() {
    let (h, mut env) = home("refused");
    let mut t = tables();
    t["profiles"]["p1"]["person"] = json!("kai");
    h.tables(&t);
    env.insert("KITH_PERSON".to_owned(), "sam".to_owned());
    let mut c = h.ctx();
    c.person = Some("sam".to_owned());
    let made = mcp_config(&c, &env, "echo", &Starter::Script).unwrap();
    assert_eq!(made.config, json!({"mcpServers": {}}));
    assert_eq!(
        made.notes,
        [
            "kith: echo: svca left out this run: p1 is kai's and this run is for sam (kith-auth grant echo@sam svca <their profile>)"
        ]
    );
}

#[test]
fn the_tombstone_gives_the_same_config_from_the_core_s_view() {
    let (h, env) = home("tombstone");
    let (from_file, _) = config(&h, &env, &Starter::Script);
    h.write("home/.kith/secrets/auth.json", NOTE);
    let fake = Fake::answering(json!({"auth": tables()}));
    test_client(fake.clone(), h.auth());
    let (from_core, notes) = config(&h, &env, &Starter::Script);
    assert_eq!((from_core, notes.len()), (from_file, 0));
    assert_eq!(fake.asked()[0].0, "people.secretsView");
    // the core gone: the refusal, no servers and no guess
    test_client(Fake::failing(false, "no core daemon answers"), h.auth());
    let d = mcp_config(&h.ctx(), &env, "echo", &Starter::Script).unwrap_err();
    assert_eq!(
        (d.code, d.msg.as_str()),
        (
            3,
            "authority moved to the core, and it gave no answer: no core daemon answers"
        )
    );
}

#[test]
fn a_browser_service_makes_the_profile_folder_private_and_sets_its_variables() {
    use std::os::unix::fs::PermissionsExt as _;
    let (h, env) = home("browser");
    h.service(
        "svcb",
        &json!({"kind": "mcp", "secrets": [], "browsers": ["chrome"],
                "mcp": {"command": "b", "args": ["--profile", "$KITH_BROWSER_PROFILE", "$KITH_BROWSER"]}}),
    );
    h.agent("echo", &["svcb"]);
    let (text, notes) = config(&h, &env, &Starter::Script);
    assert_eq!(notes.len(), 0, "{notes:?}");
    let folder = h.0.join("home/.kith/browsers/echo");
    assert!(
        text.contains(&format!(
            "\"--profile\", \"{}\", \"chrome\"",
            path_str(&folder)
        )),
        "{text}"
    );
    let mode = std::fs::metadata(folder.parent().unwrap())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    // a browser the service can't drive: skipped, with the reason
    let mut t = tables();
    t["profiles"]["browser/echo"] = json!({"browser": "firefox"});
    t["grants"]["echo"]["browser"] = json!("browser/echo");
    h.tables(&t);
    let (text, notes) = config(&h, &env, &Starter::Script);
    assert_eq!(text, "{\"mcpServers\": {}}");
    assert_eq!(
        notes,
        [
            "kith-auth: svcb can't drive firefox, the browser of browser/echo (echo's profile; kith-auth browser)",
            "kith: echo: svcb skipped this run"
        ]
    );
}

#[test]
fn a_keychain_value_is_read_through_the_security_tool_and_left_out_when_it_hangs_or_fails() {
    use std::os::unix::fs::PermissionsExt as _;
    let (h, mut env) = home("keychain");
    let mut t = tables();
    t["profiles"]["p1"]["TOKEN"] = json!("keychain:kith/p1#TOKEN");
    h.tables(&t);
    let stub = h.0.join("security");
    std::fs::write(&stub, "#!/bin/sh\n[ \"$1\" = find-generic-password ] && [ \"$2 $3 $4 $5 $6\" = \"-s kith/p1 -a TOKEN -w\" ] && echo value\n").unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    env.insert("KITH_SECURITY_BIN".to_owned(), path_str(&stub));
    let (text, notes) = config(&h, &env, &Starter::Script);
    assert!(text.contains("\"svca\""), "{text} {notes:?}");
    std::fs::write(&stub, "#!/bin/sh\nexit 44\n").unwrap();
    let (text, notes) = config(&h, &env, &Starter::Script);
    assert_eq!(text, "{\"mcpServers\": {}}");
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(
        notes[0].contains("TOKEN does not resolve (keychain:kith/p1#TOKEN;"),
        "{notes:?}"
    );
}
