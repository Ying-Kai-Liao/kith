//! The command line (section 9.2). Headless: nothing here prompts.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "kith",
    version,
    about = "Ask the agents on a Kith computer from a cloud bot's VM",
    long_about = "Join a Kith computer with an invite link once (`kith join <link>`), then ask its agents: `kith ask @agent \"question\"`.\n\
                  Results go to stdout, progress to stderr. Exit codes: 0 ok, 2 usage, 3 not joined, 4 invite problem, 5 waiting for the owner's allow, \
                  6 access removed, 7 busy or rate limited, 8 waiting for a tool approval, 9 timeout, 10 computer offline, 11 network, \
                  12 identity changed, 13 update kith."
)]
pub struct Cli {
    /// One JSON object per line on stdout instead of text.
    #[arg(long, global = true)]
    pub json: bool,
    /// Where `device.key` and `guest.json` live (default `$XDG_CONFIG_HOME/kith`, else `~/.config/kith`).
    #[arg(long, global = true, env = "KITH_GUEST_HOME", hide = true)]
    pub home: Option<PathBuf>,
    /// Use this cloud instead of the one in the link or config.
    #[arg(long, global = true, env = "KITH_CLOUD_URL", hide = true)]
    pub cloud: Option<String>,
    /// base64 of `{device, guest}` (from `kith join --credentials-file`): replaces the files, for a VM that loses them.
    #[arg(long, global = true, env = "KITH_GUEST_CREDENTIALS", hide = true, hide_env_values = true)]
    pub credentials: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Join a Kith computer with an invite link.
    Join(JoinArgs),
    /// Ask an agent and print its answer.
    Ask(AskArgs),
    /// Read a conversation (catch up on, or wait for, an answer).
    Read(ReadArgs),
    /// List this bot's conversations.
    Threads,
    /// Stop the running turn of a conversation.
    Stop(StopArgs),
    /// Read or post in a room this bot is a member of (what a woken bot runs on its turn).
    #[command(subcommand)]
    Room(RoomCommand),
    /// List the agents this bot may ask.
    Agents,
    /// Show this bot's state and whether the computer is reachable.
    Status,
    /// Remove this bot from the computer and delete its local key.
    Leave,
}

#[derive(Args, Debug)]
pub struct JoinArgs {
    /// The invite link, in single quotes (it contains `#` and `&`).
    pub link: String,
    /// The bot's name (default: this machine's hostname).
    #[arg(long)]
    pub name: Option<String>,
    /// Who runs this bot: grok, muse, dots, codex, cursor, other.
    #[arg(long)]
    pub provider: Option<String>,
    /// Seconds to wait for the computer and for the owner's allow; 0 returns right after the cloud accepts.
    #[arg(long, value_name = "SECS", default_value_t = 90)]
    pub wait: u64,
    /// Join again with a new invite even though this bot is already joined; keeps the name.
    #[arg(long)]
    pub rekey: bool,
    /// Refused: the bot's key never goes to stdout. Use --credentials-file.
    #[arg(long, hide = true)]
    pub print_credentials: bool,
    /// Write this bot's credentials line to a NEW file (mode 0600) after joining; the line never reaches stdout.
    #[arg(long, value_name = "PATH")]
    pub credentials_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct AskArgs {
    /// The agent, `@house` or `house`.
    pub agent: String,
    /// The question; `-` (or nothing, with piped input) reads it from stdin.
    pub text: Option<String>,
    /// Continue this conversation (`cv_...`).
    #[arg(long, value_name = "CONVERSATION", conflicts_with_all = ["ref", "room"])]
    pub thread: Option<String>,
    /// Find or make the conversation by the provider's own id.
    #[arg(long = "ref", value_name = "EXTERNAL_ID", conflicts_with = "room")]
    pub r#ref: Option<String>,
    /// Ask in a room (`rm_...`).
    #[arg(long, value_name = "ROOM")]
    pub room: Option<String>,
    /// Who is talking through this bot (a label the host cannot verify).
    #[arg(long)]
    pub speaker: Option<String>,
    /// Print the answer as it grows (default when stdout is a terminal).
    #[arg(long, conflicts_with = "wait")]
    pub stream: bool,
    /// Print the answer once, at the end (default otherwise).
    #[arg(long)]
    pub wait: bool,
    /// Seconds to wait for the answer.
    #[arg(long, value_name = "SECS", default_value_t = 600)]
    pub timeout: u64,
    /// A key that makes a repeated ask (even from a new process) send once.
    #[arg(long, value_name = "KEY")]
    pub idempotency_key: Option<String>,
    /// Fail with exit 7 instead of queueing behind a running turn.
    #[arg(long)]
    pub no_queue: bool,
}

#[derive(Args, Debug)]
pub struct ReadArgs {
    pub conversation: String,
    /// Only entries after this sequence number.
    #[arg(long, value_name = "SEQ")]
    pub after: Option<u64>,
    /// Wait for the running turn to finish.
    #[arg(long)]
    pub wait: bool,
    #[arg(long, value_name = "SECS", default_value_t = 600)]
    pub timeout: u64,
}

#[derive(Args, Debug)]
pub struct StopArgs {
    pub conversation: String,
}

#[derive(Subcommand, Debug)]
pub enum RoomCommand {
    /// Print what a room said since a cursor, and the new cursor.
    Read(RoomReadArgs),
    /// Post one message into a room (a reply to your turn, or speaking up).
    Post(RoomPostArgs),
}

#[derive(Args, Debug)]
pub struct RoomReadArgs {
    /// The room (a group's id, as the wake names it).
    pub room: String,
    /// Only entries after this cursor (the `since` of the wake, or the `cursor` the last read printed).
    #[arg(long, value_name = "CURSOR")]
    pub since: Option<u64>,
}

#[derive(Args, Debug)]
pub struct RoomPostArgs {
    /// The room.
    pub room: String,
    /// The conversation the wake named.
    #[arg(long, value_name = "CONVERSATION")]
    pub conversation: String,
    /// The epoch the wake named.
    #[arg(long, value_name = "EPOCH")]
    pub epoch: u64,
    /// The turn key the wake named: with it this post is your reply to your turn.
    #[arg(long, value_name = "KEY")]
    pub turn_key: Option<String>,
    /// A key that makes a repeated post (even from a new process) post once; default: a hash of the post.
    #[arg(long, value_name = "KEY")]
    pub nonce: Option<String>,
    /// The message; `-` reads it from stdin. `(pass)` when you have nothing to add.
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("kith").chain(args.iter().copied()))
    }

    #[test]
    fn the_documented_commands_parse() {
        assert!(matches!(parse(&["join", "https://x/b/c/i#t=1&k=2"]).unwrap().command, Command::Join(_)));
        let Command::Join(j) =
            parse(&["join", "L", "--wait", "0", "--rekey", "--credentials-file", "/w/c", "--name", "Rex"])
                .unwrap()
                .command
        else {
            panic!()
        };
        assert_eq!((j.wait, j.rekey, j.credentials_file.is_some(), j.name.as_deref()), (0, true, true, Some("Rex")));

        let cli =
            parse(&["ask", "@house", "--thread", "cv_1", "--json", "--timeout", "5", "--no-queue", "what?"]).unwrap();
        assert!(cli.json);
        let Command::Ask(a) = cli.command else { panic!() };
        assert_eq!((a.agent.as_str(), a.thread.as_deref(), a.timeout, a.no_queue), ("@house", Some("cv_1"), 5, true));
        assert_eq!(a.text.as_deref(), Some("what?"));

        let Command::Ask(a) = parse(&["ask", "house", "-"]).unwrap().command else { panic!() };
        assert_eq!(a.text.as_deref(), Some("-"));
        for c in ["threads", "agents", "status", "leave"] {
            parse(&[c]).unwrap();
        }
        parse(&["read", "cv_1", "--after", "4", "--wait"]).unwrap();
        parse(&["stop", "cv_1"]).unwrap();
        parse(&["room", "read", "rm_1", "--since", "12", "--json"]).unwrap();
        parse(&["room", "post", "rm_1", "--conversation", "c", "--epoch", "2", "--turn-key", "k", "hi"]).unwrap();
    }

    #[test]
    fn conflicting_flags_are_usage_errors() {
        for args in [
            &["ask", "house", "--stream", "--wait", "x"][..],
            &["ask", "house", "--thread", "cv_1", "--ref", "x", "y"][..],
            &["ask", "house", "--thread", "cv_1", "--room", "rm_1", "y"][..],
            &["ask"][..],
            &["room", "post", "rm_1", "--epoch", "2", "hi"][..],
            &["room", "listen", "rm_1"][..],
            &["nonsense"][..],
        ] {
            let e = parse(args).unwrap_err();
            assert!(e.use_stderr(), "{args:?}");
        }
    }
}
