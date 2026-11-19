//! The permission card's readings: the three calls on the designer's board, the safety rule (a fact never makes a call
//! look safer than it is) and the plain line made of an agent's description.

use super::intent::*;
use serde_json::{Value, json};
use std::path::PathBuf;

fn places() -> Places {
    Places::new(
        PathBuf::from("/home/kai"),
        PathBuf::from("/home/kai/kith/agents"),
        PathBuf::from("/home/kai/kith"),
        "video",
    )
}

fn bash(command: &str) -> Reading {
    reading("Bash", &json!({"command": command}), &places()).expect("a reading")
}

fn texts(r: &Reading) -> Vec<String> {
    r.facts.iter().map(|f| f.text.clone()).collect()
}

fn says_reads_only(r: &Reading) -> bool {
    r.facts.iter().any(|f| f.text.contains("reads only"))
}

const CANT_READ: &str = "A shell command Kith can't read in full";

// ---- the three calls on cards.png

#[test]
fn card_a_a_python_script_and_a_package() {
    let r = bash(
        "cd /home/kai/Workspace/design/_drafts/event-reel/video; uv run -q --with faster-whisper python -I - <<'EOF'\n\
         import sys\nprint(sys.argv)\nEOF\nffmpeg -v error -i a.wav b.wav",
    );
    assert_eq!(
        texts(&r),
        [
            "Runs a Python script written into the command, and ffmpeg",
            "Fetches the faster-whisper package for this run",
            "In event-reel/video"
        ]
    );
    assert_eq!(r.facts[0].strong, ["Python script", "ffmpeg"]);
    assert_eq!(r.facts[1].strong, ["faster-whisper"]);
    assert!(r.facts[2].place && r.facts[2].strong == ["event-reel/video"]);
    assert!(!r.open_details);
}

#[test]
fn card_b_a_loop_of_downloads() {
    let r = bash(
        "cd /home/kai/Workspace/design/kith/video/music-candidates && UA=\"Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)\"; \
         for h in chill-upbeat-summer-vlog summer-tropical-house back-to-the-sea-1 maalhos mirihi waves-8; do \
         curl -sL -A \"$UA\" -e \"https://freemusicarchive.org/\" \"https://freemusicarchive.org/track/$h/stream/\" -o \"dl/$h.mp3\"; \
         echo \"$h $(file -b dl/$h.mp3 | cut -c1-6)\"; done",
    );
    assert_eq!(
        texts(&r),
        [
            "A shell command: curl, six times",
            "Reaches freemusicarchive.org",
            "Saves into music-candidates/dl",
            "In video/music-candidates"
        ]
    );
    assert_eq!(r.facts[1].strong, ["freemusicarchive.org"]);
}

#[test]
fn card_c_a_read_only_grep() {
    let r = bash("grep -n 'tick-lock' /home/kai/.kith/cli/0.1.4/share/kith/kernel/bin/kith-sched");
    assert_eq!(texts(&r), ["A shell command: grep, reads only", "Reads ~/.kith/cli"]);
    assert_eq!(r.facts[0].strong, ["A shell command"]);
    assert_eq!(r.facts[1].muted.as_deref(), Some("Kith's installed program"));
}

// ---- a fact never makes a call look safer

#[test]
fn reads_only_is_left_out_unless_every_stage_is_known_and_plain() {
    for cmd in [
        "grep x f | sh",
        "grep x f > out.txt",
        "grep x f >> out.txt",
        "grep x f | tee out.txt",
        "grep -o x f",
        "cat $(echo f)",
        "cat `echo f`",
        "grep x f | python3 -",
        "cat f | bash",
        "sed -i '' s/a/b/ f",
        "ls | xargs rm",
        "eval 'ls'",
        "find . -delete",
        "find . -exec rm {} +",
        "sort -o out f",
        "git diff --output=x",
        "rg --pre ./x y",
        "curl -sL https://x.org",
    ] {
        let r = bash(cmd);
        assert!(!says_reads_only(&r), "{cmd:?} must not say reads only: {:?}", texts(&r));
    }
}

#[test]
fn reads_only_when_it_is_so() {
    for cmd in [
        "ls -la",
        "cat a.txt | head -5 | wc -l",
        "git log --oneline | head",
        "find . -name '*.rs' | grep x",
        "grep -c x f 2>&1",
    ] {
        let r = bash(cmd);
        assert!(says_reads_only(&r), "{cmd:?}: {:?}", texts(&r));
    }
}

#[test]
fn what_it_cannot_follow_is_one_row_and_details_open() {
    for cmd in [
        "eval \"$x\"",
        "ls | xargs rm",
        "find . -exec rm {} +",
        "echo 'unbalanced",
        "uv run --mystery x.py",
        "python3 --mystery x.py",
        "pkill --weird x",
        "a; b; c; d; e",
        "cat <(ls)",
    ] {
        let r = bash(cmd);
        assert_eq!(texts(&r), [CANT_READ], "{cmd}");
        assert!(r.open_details, "{cmd}");
    }
    let long = format!("echo {}", "x".repeat(5000));
    let r = bash(&long);
    assert_eq!(texts(&r), [CANT_READ]);
    assert!(r.open_details);
}

#[test]
fn a_piped_in_shell_is_named_for_what_it_is() {
    let r = bash("grep x f | sh");
    assert_eq!(texts(&r), ["Runs whatever is piped into sh"]);
}

// ---- the other rows

#[test]
fn deletes_and_stops_come_first() {
    let r = bash("cd /home/kai/kith/agents/video/files && python3 run.py; rm -rf build; pkill -f tlc2.TLC");
    let t = texts(&r);
    assert_eq!(t[0], "Deletes files/build and what is in it");
    assert_eq!(t[1], "Stops processes named tlc2.TLC");
    assert_eq!(t[2], "Runs a Python script");
    assert_eq!(r.facts[3].muted.as_deref(), Some("its folder"));
}

#[test]
fn a_non_recursive_delete_names_the_folder() {
    let r = bash("cd /home/kai/x/proj && rm notes.txt old.txt");
    assert_eq!(texts(&r)[0], "Deletes files in x/proj");
}

#[test]
fn packages_and_sites() {
    let r = bash("npx -y cowsay hello");
    assert_eq!(texts(&r), ["A shell command: cowsay", "Fetches the cowsay package for this run"]);
    let r = bash("pip install requests rich");
    assert!(texts(&r).contains(&"Fetches the requests and rich packages".to_owned()));
    let r = bash("curl -s https://a.org/x https://b.org/y https://a.org/z");
    assert!(texts(&r).contains(&"Reaches a.org, b.org".to_owned()));
}

#[test]
fn an_echo_does_not_reach_the_site_it_mentions() {
    let r = bash("echo see https://example.org");
    assert!(texts(&r).iter().all(|t| !t.starts_with("Reaches")));
}

#[test]
fn saves_come_from_redirects_tee_sed_and_curl() {
    let r = bash("cd /home/kai/a/b && echo hi > out.txt");
    assert!(texts(&r).contains(&"Saves into a/b".to_owned()));
    let r = bash("cd /home/kai/a/b && sed -i '' 's/x/y/' src/f.rs");
    assert!(texts(&r).contains(&"Saves into b/src".to_owned()));
    let r = bash("cd /home/kai/a/b && echo hi 2>&1 | tee log/x.txt");
    assert!(texts(&r).contains(&"Saves into b/log".to_owned()));
    let r = bash("echo hi > /dev/null 2>&1");
    assert!(texts(&r).iter().all(|t| !t.starts_with("Saves")));
}

#[test]
fn the_folder_is_labelled_for_this_agent() {
    let r = bash("cd /home/kai/.kith/cli/0.1/share && ls");
    let w = r.facts.last().unwrap();
    assert_eq!((w.text.as_str(), w.muted.as_deref()), ("In ~/.kith/cli", Some("Kith's installed program")));
    let r = bash("cd /home/kai/kith/agents/architect/files && ls");
    assert_eq!(r.facts.last().unwrap().muted.as_deref(), Some("architect's folder"));
    let r = bash("cd /home/kai/kith/serve && ls");
    assert_eq!(r.facts.last().unwrap().muted.as_deref(), Some("the kith repo"));
    // A `cd` after something else says nothing about where it ran.
    let r = bash("ls; cd /home/kai/x");
    assert!(r.facts.iter().all(|f| !f.place));
}

#[test]
fn no_more_than_four_rows() {
    let r = bash("cd /home/kai/a/b && curl -sL https://x.org/y -o z.bin; rm -rf q; python3 t.py");
    assert!(r.facts.len() <= 4, "{:?}", texts(&r));
    assert!(!r.facts.iter().any(|f| f.text == CANT_READ));
}

// ---- other tools

#[test]
fn other_tools_state_what_they_name() {
    let p = places();
    let one = |tool: &str, input: Value| reading(tool, &input, &p).map(|r| r.facts[0].text.clone());
    assert_eq!(
        one("Edit", json!({"file_path": "/home/kai/kith/agents/video/files/a.md"})).as_deref(),
        Some("Changes files/a.md")
    );
    assert_eq!(one("Read", json!({"file_path": "/home/kai/x/y/z.txt"})).as_deref(), Some("Reads y/z.txt"));
    assert_eq!(one("WebFetch", json!({"url": "https://Docs.Rs/x"})).as_deref(), Some("Opens docs.rs"));
    assert_eq!(one("mcp__slack__post", json!({})).as_deref(), Some("Uses slack · post"));
    assert_eq!(one("TodoWrite", json!({})), None);
}

// ---- the agent's words

#[test]
fn the_intent_is_one_plain_line_of_at_most_200_characters() {
    let say = |d: &str| intent("Bash", &json!({"command": "ls", "description": d}));
    assert_eq!(
        say("Transcribing the clips\nso I can find\r\n  each answer."),
        Some("Transcribing the clips so I can find each answer.".into())
    );
    assert_eq!(
        say("**Bold** and `code` and [a link](https://evil.example/x) here"),
        Some("Bold and code and a link here".into())
    );
    assert_eq!(say("# Heading\n> quote").as_deref(), Some("Heading > quote"));
    assert_eq!(
        say("Fetch https://freemusicarchive.org/search?q=a now").as_deref(),
        Some("Fetch freemusicarchive.org now")
    );
    assert_eq!(say(&"x".repeat(500)).unwrap().chars().count(), 200);
    assert_eq!(say("   \n  "), None);
    assert_eq!(intent("Bash", &json!({"command": "ls"})), None);
    assert_eq!(intent("Bash", &json!({"command": "ls", "description": 7})), None);
    // Only a Bash call has one.
    assert_eq!(intent("Write", &json!({"file_path": "/x", "description": "hi"})), None);
}

#[test]
fn annotate_adds_only_what_there_is() {
    let mut data = json!({"title": "Run ls"});
    annotate(&mut data, "Bash", &json!({"command": "ls", "description": "Look around"}), "video", None);
    assert_eq!(data["intent"], "Look around");
    assert_eq!(data["facts"][0]["text"], "A shell command: ls, reads only");
    assert!(data.get("check").is_none() && data.get("detailsOpen").is_none());

    let mut none = json!({});
    annotate(&mut none, "TodoWrite", &json!({}), "video", Some("[Code from External]"));
    assert_eq!(none, json!({}), "a call with nothing to say keeps the plain card");

    let mut auto = json!({});
    annotate(&mut auto, "Bash", &json!({"command": "eval x"}), "video", Some("[Bot Check Circumvention]"));
    assert_eq!(auto["facts"][0]["text"], CANT_READ);
    assert_eq!(auto["detailsOpen"], true);
    assert_eq!(
        auto["check"],
        "Safety check stopped it: it presents itself as a web browser to get past the site's bot check"
    );
}

// ---- the check

#[test]
fn the_classifier_tag_is_a_sentence_and_no_explanation_is_none() {
    let line = |r: &str| check(r);
    assert_eq!(line("[Code from External]").as_deref(), Some("Safety check stopped it: runs code it downloaded"));
    assert_eq!(
        line("[Interfere With Workloads]").as_deref(),
        Some("Safety check stopped it: it stops programs that are running")
    );
    assert_eq!(
        line("[Create Unsafe Agents]").as_deref(),
        Some("Safety check stopped it: it starts an agent with fewer checks")
    );
    assert_eq!(
        line("The server-side auto mode classifier judged this action dangerous (it gave no explanation)"),
        None
    );
    assert_eq!(line("[Something New]"), None);
    assert_eq!(line(""), None);
}
