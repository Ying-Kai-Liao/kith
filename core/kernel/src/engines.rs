//! The coding engines Kith can run an agent on, as one table (docs/plans/core-design.md section 7). The host's 27
//! harnesses are the base; the kernel's `harness.py` engines that the host does not know (hermes, dsh) are rows too,
//! with no ACP mode. Data plus pure functions: detection takes the lookup of a program on PATH as an argument, so the
//! host (with its hydrated PATH) and the CLI (with the plain one) share the rows and keep their own search.
//!
//! Nothing here starts a process. The host runs the sign-in probes and the install and login commands in its own
//! terminal; `kith engines install|login <id>` prints them.

use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::PathBuf;

/// One engine.
pub struct Engine {
    /// Stable: stored bots and agents name an engine by it.
    pub id: &'static str,
    /// Other names that mean this engine (the kernel's `agy` is the host's `antigravity`).
    pub aliases: &'static [&'static str],
    pub name: &'static str,
    /// CLI names that mean "installed on this machine", best first.
    pub bins: &'static [&'static str],
    /// The installed CLI's native ACP mode (`{bin}` = resolved binary path).
    pub local: Option<&'static str>,
    /// Entry in the ACP registry (adapter or pinned build).
    pub registry: Option<&'static str>,
    /// What to tell someone setting it up by hand.
    pub setup: &'static str,
    /// How Kith installs the CLI for you.
    pub install: Option<Install>,
    /// Shell command that signs in (`{bin}` = the CLI); empty: not known.
    pub login: &'static str,
    /// How to tell whether it's signed in without starting it.
    pub signed_in: Option<SignInCheck>,
    /// Where the model list comes from.
    pub models: Models,
    /// How a one-shot run (`kith ask`) talks to it.
    pub oneshot: Oneshot,
    /// Environment variables the engine reads for its own key (feeds `secrets`).
    pub env: &'static [&'static str],
    /// What Kith can hold the engine to. Unknown is not enforced.
    pub enforces: Enforces,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    /// A vendor install script (`curl … | bash`), Unix only.
    Script(&'static str),
    /// Vendor install scripts for Unix and for Windows (PowerShell).
    Scripts {
        unix: &'static str,
        windows: &'static str,
    },
    /// A global npm package, installed on the computer's Node or else Kith's.
    Npm(&'static str),
}

impl Install {
    /// The command to run on this platform; `npm` turns a package into the install command (the host runs it on
    /// Kith's own Node when the computer has none). `None`: nothing Kith can run here.
    pub fn command(self, unix: bool, npm: &dyn Fn(&str) -> Option<String>) -> Option<String> {
        match self {
            Self::Script(s) | Self::Scripts { unix: s, .. } if unix => Some(s.to_owned()),
            Self::Script(_) => None,
            Self::Scripts { windows, .. } => Some(windows.to_owned()),
            Self::Npm(pkg) => npm(pkg),
        }
    }
}

/// `<bin> <args>`, judged by exit code and stdout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignInCheck {
    pub args: &'static str,
    pub ok: Predicate,
}

/// What the probe's answer must be for "signed in".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Predicate {
    /// The command exits 0.
    ExitOk,
    /// Stdout is a JSON object whose top-level key is `true`.
    JsonTrue(&'static str),
    /// Stdout says "logged in" and not "not logged in".
    SaysLoggedIn,
}

impl Predicate {
    pub fn eval(self, success: bool, out: &str) -> bool {
        match self {
            Self::ExitOk => success,
            Self::JsonTrue(key) => serde_json::from_str::<Value>(out).is_ok_and(|v| v[key] == true),
            Self::SaysLoggedIn => {
                let out = out.to_lowercase();
                out.contains("logged in") && !out.contains("not logged in")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Models {
    /// The ACP session's config options.
    Acp,
    /// Not listed (the engine picks its own, or a flag names one).
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Oneshot {
    /// Over ACP: open a session, send the task, collect the answer (A2b).
    Acp,
    /// The engine's own one-shot command (`{bin}` = the CLI): claude until ACP is shown to report the same cost as
    /// `claude -p --output-format json`, and engines with no ACP mode.
    Native(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RolePrompt {
    /// Not known to be deliverable.
    None,
    /// A flag or config option on the command line.
    Flag,
    /// An environment variable.
    Env,
    /// In front of the first message.
    FirstMessage,
}

impl RolePrompt {
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Flag => "flag",
            Self::Env => "env",
            Self::FirstMessage => "first-message",
        }
    }
}

/// What Kith can make an engine do for a run. A guest run needs the hooks; an engine without them is not offered to guests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // one yes/no per thing Kith can enforce; a flags type would say less
pub struct Enforces {
    /// Tool hooks (`PreToolUse` deny, `PermissionDenied`, the ledger) written into the engine's settings.
    pub tool_hooks: bool,
    /// The engine takes a permission mode (ask, auto-edit, bypass) from Kith.
    pub permission_modes: bool,
    /// Per-run MCP servers (the ones an agent is granted).
    pub per_run_mcp: bool,
    pub role_prompt: RolePrompt,
    /// `kith <agent>` in a terminal can start the engine's own TUI with the run's flags.
    pub interactive: bool,
}

impl Enforces {
    pub const NONE: Self = Self {
        tool_hooks: false,
        permission_modes: false,
        per_run_mcp: false,
        role_prompt: RolePrompt::None,
        interactive: false,
    };
    pub const CLAUDE: Self = Self {
        tool_hooks: true,
        permission_modes: true,
        per_run_mcp: true,
        role_prompt: RolePrompt::Flag,
        interactive: true,
    };
}

impl Engine {
    /// Speaks ACP one way or the other.
    pub fn has_acp(&self) -> bool {
        self.local.is_some() || self.registry.is_some()
    }

    /// `local`, `registry`, both or neither, as the table lists it.
    pub fn acp_modes(&self) -> Vec<&'static str> {
        [
            self.local.map(|_| "local"),
            self.registry.map(|_| "registry"),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The sign-in command with `{bin}` resolved. Empty when none is known.
    pub fn login_command(&self, bin: &str) -> String {
        self.login.replace("{bin}", bin)
    }
}

/// An engine by id or alias.
pub fn find(name: &str) -> Option<&'static Engine> {
    ENGINES
        .iter()
        .find(|e| e.id == name || e.aliases.contains(&name))
}

/// An engine by its id only: what a stored bot's backend names.
pub fn by_id(id: &str) -> Option<&'static Engine> {
    ENGINES.iter().find(|e| e.id == id)
}

/// The first of the engine's programs the lookup finds: the name it was found as, and where.
pub fn detect(
    e: &Engine,
    which: &dyn Fn(&str) -> Option<PathBuf>,
) -> Option<(&'static str, PathBuf)> {
    e.bins.iter().find_map(|b| which(b).map(|p| (*b, p)))
}

/// One engine as `kith engines --json` lists it. `signed_in` is `null`: the probes run programs, which the host does
/// on its own schedule and this crate never.
pub fn describe(e: &Engine, which: &dyn Fn(&str) -> Option<PathBuf>) -> Value {
    let found = detect(e, which);
    json!({
        "id": e.id,
        "name": e.name,
        "aliases": e.aliases,
        "installed": found.is_some(),
        "path": found.map(|(_, p)| p.to_string_lossy().into_owned()),
        "signed_in": null,
        "acp": e.acp_modes(),
        "oneshot": match e.oneshot {
            Oneshot::Acp => json!("acp"),
            Oneshot::Native(_) => json!("native"),
        },
        "enforces": {
            "tool_hooks": e.enforces.tool_hooks,
            "permission_modes": e.enforces.permission_modes,
            "per_run_mcp": e.enforces.per_run_mcp,
            "role_prompt": e.enforces.role_prompt.name(),
            "interactive": e.enforces.interactive,
        },
    })
}

/// `kith engines --json`.
pub fn list_json(which: &dyn Fn(&str) -> Option<PathBuf>) -> String {
    let engines: Vec<Value> = ENGINES.iter().map(|e| describe(e, which)).collect();
    json!({ "engines": engines }).to_string()
}

/// `kith engines`: one line per engine.
pub fn list_text(which: &dyn Fn(&str) -> Option<PathBuf>) -> String {
    let mut out = String::new();
    for e in ENGINES {
        let state = if detect(e, which).is_some() {
            "installed"
        } else {
            "not installed"
        };
        let _ = writeln!(out, "{:<12} {:<22} {state}", e.id, e.name);
    }
    out
}

/// The command `kith engines install <id>` prints, for this platform.
pub fn install_text(e: &Engine, unix: bool) -> Result<String, String> {
    let npm = |pkg: &str| Some(format!("npm install -g {pkg}"));
    e.install
        .and_then(|i| i.command(unix, &npm))
        .ok_or_else(|| {
            format!(
                "No install command for {} on this platform. {}",
                e.name, e.setup
            )
        })
}

/// The command `kith engines login <id>` prints; `{bin}` is the program found, else the engine's first name.
pub fn login_text(e: &Engine, which: &dyn Fn(&str) -> Option<PathBuf>) -> Result<String, String> {
    if e.login.is_empty() {
        return Err(format!(
            "No sign-in command known for {}. {}",
            e.name, e.setup
        ));
    }
    let bin = detect(e, which).map_or_else(|| e.bins[0].to_owned(), |(b, _)| b.to_owned());
    Ok(e.login_command(&bin))
}

/// Every engine. Order is the order the app lists them in.
pub const ENGINES: &[Engine] = &[
    Engine {
        id: "claude",
        aliases: &["claude-code"],
        name: "Claude Code",
        bins: &["claude"],
        local: None,
        registry: Some("claude-acp"),
        setup: "Install Claude Code and run `claude auth login`.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://claude.ai/install.sh | bash",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://claude.ai/install.ps1 | iex\"",
        }),
        login: "{bin} auth login",
        signed_in: Some(SignInCheck {
            args: "auth status",
            ok: Predicate::JsonTrue("loggedIn"),
        }),
        models: Models::Acp,
        oneshot: Oneshot::Native("{bin} -p --output-format json"),
        env: &["ANTHROPIC_API_KEY"],
        enforces: Enforces::CLAUDE,
    },
    Engine {
        id: "codex",
        aliases: &[],
        name: "Codex",
        bins: &["codex"],
        local: None,
        registry: Some("codex-acp"),
        setup: "Install Codex CLI and run `codex login --device-auth`.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://chatgpt.com/codex/install.sh | sh",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://chatgpt.com/codex/install.ps1 | iex\"",
        }),
        login: "{bin} login --device-auth",
        signed_in: Some(SignInCheck {
            args: "login status",
            ok: Predicate::ExitOk,
        }),
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &["OPENAI_API_KEY"],
        enforces: Enforces {
            tool_hooks: false,
            permission_modes: true,
            per_run_mcp: true,
            role_prompt: RolePrompt::Flag,
            interactive: true,
        },
    },
    Engine {
        id: "cursor",
        aliases: &[],
        name: "Cursor",
        bins: &["cursor-agent", "agent"],
        local: Some("{bin} acp"),
        registry: Some("cursor"),
        setup: "Install Cursor CLI (curl https://cursor.com/install -fsS | bash) and run `cursor-agent login`.",
        install: Some(Install::Scripts {
            unix: "curl https://cursor.com/install -fsS | bash",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm 'https://cursor.com/install?win32=true' | iex\"",
        }),
        login: "NO_OPEN_BROWSER=1 {bin} login",
        signed_in: Some(SignInCheck {
            args: "status",
            ok: Predicate::SaysLoggedIn,
        }),
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &["CURSOR_API_KEY"],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "pi",
        aliases: &[],
        name: "Pi",
        bins: &["pi"],
        local: None,
        registry: Some("pi-acp"),
        setup: "Install pi (npm install -g @earendil-works/pi-coding-agent), run `pi` and type /login.",
        install: Some(Install::Npm("@earendil-works/pi-coding-agent")),
        login: "{bin}",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces {
            tool_hooks: false,
            permission_modes: false,
            per_run_mcp: true,
            role_prompt: RolePrompt::Flag,
            interactive: true,
        },
    },
    Engine {
        id: "omp",
        aliases: &[],
        name: "oh-my-pi",
        bins: &["omp"],
        local: Some("{bin} acp"),
        registry: None,
        setup: "Install oh-my-pi (curl -fsSL https://omp.sh/install | sh -s -- --binary) and run `omp login`.",
        install: Some(Install::Scripts {
            // The prebuilt binary: by default the scripts build through any Bun on PATH and stop when it's older than they need.
            unix: "curl -fsSL https://omp.sh/install | sh -s -- --binary",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"& ([scriptblock]::Create((irm https://omp.sh/install.ps1))) -Binary\"",
        }),
        login: "{bin} login",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "opencode",
        aliases: &[],
        name: "OpenCode",
        bins: &["opencode"],
        local: Some("{bin} acp"),
        registry: Some("opencode"),
        setup: "Install OpenCode (curl -fsSL https://opencode.ai/install | bash) and run `opencode auth login`.",
        install: Some(Install::Script(
            "curl -fsSL https://opencode.ai/install | bash",
        )),
        login: "{bin} auth login",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces {
            tool_hooks: false,
            permission_modes: true,
            per_run_mcp: true,
            role_prompt: RolePrompt::Env,
            interactive: true,
        },
    },
    Engine {
        id: "grok",
        aliases: &[],
        name: "Grok Build",
        bins: &["grok"],
        local: Some("{bin} agent stdio"),
        registry: Some("grok-build"),
        setup: "Install Grok Build (curl -fsSL https://x.ai/cli/install.sh | bash) and run `grok login --device-auth`.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://x.ai/cli/install.sh | bash",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://x.ai/cli/install.ps1 | iex\"",
        }),
        login: "{bin} login --device-auth",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "gemini",
        aliases: &[],
        name: "Gemini CLI",
        bins: &["gemini"],
        local: Some("{bin} --acp"),
        registry: Some("gemini"),
        setup: "Install Gemini CLI (npm install -g @google/gemini-cli) and run `gemini` to sign in with Google.",
        install: Some(Install::Npm("@google/gemini-cli")),
        login: "NO_BROWSER=true {bin}",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &["GEMINI_API_KEY"],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "copilot",
        aliases: &[],
        name: "GitHub Copilot",
        bins: &["copilot"],
        local: Some("{bin} --acp --stdio"),
        registry: Some("github-copilot-cli"),
        setup: "Install Copilot CLI (curl -fsSL https://gh.io/copilot-install | bash) and run `copilot login`.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://gh.io/copilot-install | bash",
            windows: "winget install --id GitHub.Copilot -e",
        }),
        login: "{bin} login --device-code",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "qwen",
        aliases: &[],
        name: "Qwen Code",
        bins: &["qwen"],
        local: Some("{bin} --acp"),
        registry: Some("qwen-code"),
        setup: "Install Qwen Code (its standalone installer), run `qwen` and type /auth.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://qwen-code-assets.oss-cn-hangzhou.aliyuncs.com/installation/install-qwen-standalone.sh | bash",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://qwen-code-assets.oss-cn-hangzhou.aliyuncs.com/installation/install-qwen-standalone.ps1 | iex\"",
        }),
        login: "{bin}",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces {
            tool_hooks: false,
            permission_modes: true,
            per_run_mcp: false,
            role_prompt: RolePrompt::Flag,
            interactive: true,
        },
    },
    Engine {
        id: "goose",
        aliases: &[],
        name: "goose",
        bins: &["goose"],
        local: Some("{bin} acp"),
        registry: Some("goose"),
        setup: "Install goose and run `goose configure`.",
        install: Some(Install::Script(
            "curl -fsSL https://github.com/aaif-goose/goose/releases/download/stable/download_cli.sh | bash",
        )),
        login: "{bin} configure",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "kimi",
        aliases: &[],
        name: "Kimi Code",
        bins: &["kimi"],
        local: Some("{bin} acp"),
        registry: Some("kimi"),
        setup: "Install Kimi Code (curl -fsSL https://code.kimi.com/kimi-code/install.sh | bash) and run `kimi login`.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://code.kimi.com/kimi-code/install.sh | bash",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://code.kimi.com/kimi-code/install.ps1 | iex\"",
        }),
        login: "{bin} login",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "droid",
        aliases: &[],
        name: "Factory Droid",
        bins: &["droid"],
        local: Some("{bin} exec --output-format acp-daemon"),
        registry: Some("factory-droid"),
        setup: "Install Droid (curl -fsSL https://app.factory.ai/cli | sh) and sign in with `droid`.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://app.factory.ai/cli | sh",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://app.factory.ai/cli/windows | iex\"",
        }),
        login: "{bin}",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "amp",
        aliases: &[],
        name: "Amp",
        bins: &["amp"],
        local: None,
        registry: Some("amp-acp"),
        setup: "Install Amp (curl -fsSL https://ampcode.com/install.sh | bash) and run `amp login`.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://ampcode.com/install.sh | bash",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://ampcode.com/install.ps1 | iex\"",
        }),
        login: "{bin} login",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "kilo",
        aliases: &[],
        name: "Kilo",
        bins: &["kilo"],
        local: Some("{bin} acp"),
        registry: Some("kilo"),
        setup: "Install Kilo CLI (npm install -g @kilocode/cli) and run `kilo auth login`.",
        install: Some(Install::Npm("@kilocode/cli")),
        login: "{bin} auth login",
        signed_in: Some(SignInCheck {
            args: "profile --json",
            ok: Predicate::ExitOk,
        }),
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "cline",
        aliases: &[],
        name: "Cline",
        bins: &["cline"],
        local: Some("{bin} --acp"),
        registry: Some("cline"),
        setup: "Install Cline CLI (npm install -g cline) and run `cline auth`.",
        install: Some(Install::Npm("cline")),
        login: "{bin} auth",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "auggie",
        aliases: &[],
        name: "Auggie",
        bins: &["auggie"],
        local: Some("{bin} --acp"),
        registry: Some("auggie"),
        setup: "Install Auggie (npm install -g @augmentcode/auggie) and run `auggie login`.",
        install: Some(Install::Npm("@augmentcode/auggie")),
        login: "{bin} login",
        signed_in: Some(SignInCheck {
            args: "account status",
            ok: Predicate::ExitOk,
        }),
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "vibe",
        aliases: &[],
        name: "Mistral Vibe",
        bins: &["vibe-acp", "vibe"],
        local: None,
        registry: Some("mistral-vibe"),
        setup: "Install Mistral Vibe (curl -LsSf https://mistral.ai/vibe/install.sh | bash) and run `vibe --setup`.",
        install: Some(Install::Script(
            "curl -LsSf https://mistral.ai/vibe/install.sh | bash",
        )),
        login: "vibe --setup",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "kiro",
        aliases: &[],
        name: "Kiro CLI",
        bins: &["kiro-cli"],
        local: Some("{bin} acp"),
        registry: None,
        setup: "Install Kiro CLI (curl -fsSL https://cli.kiro.dev/install | bash) and run `kiro-cli login`.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://cli.kiro.dev/install | bash",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm 'https://cli.kiro.dev/install.ps1' | iex\"",
        }),
        login: "{bin} login",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "devin",
        aliases: &[],
        name: "Devin",
        bins: &["devin"],
        local: Some("{bin} acp"),
        registry: Some("devin"),
        setup: "Install the Devin CLI (curl -fsSL https://cli.devin.ai/install.sh | bash) and run `devin auth login`.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://cli.devin.ai/install.sh | bash",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://cli.devin.ai/install.ps1 | iex\"",
        }),
        login: "{bin} auth login",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "qoder",
        aliases: &[],
        name: "Qoder CLI",
        bins: &["qodercli"],
        local: Some("{bin} --acp"),
        registry: Some("qoder"),
        setup: "Install Qoder CLI (curl -fsSL https://qoder.com/install | bash) and run `qodercli login`.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://qoder.com/install | bash",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://qoder.com/install.ps1 | iex\"",
        }),
        login: "{bin} login",
        signed_in: Some(SignInCheck {
            args: "status -o json",
            ok: Predicate::JsonTrue("logged_in"),
        }),
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "codebuddy",
        aliases: &[],
        name: "CodeBuddy Code",
        bins: &["codebuddy", "cbc"],
        local: Some("{bin} --acp"),
        registry: Some("codebuddy-code"),
        setup: "Install CodeBuddy Code (npm install -g @tencent-ai/codebuddy-code) and sign in with `codebuddy`.",
        install: Some(Install::Npm("@tencent-ai/codebuddy-code")),
        login: "{bin}",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "minimax",
        aliases: &[],
        name: "MiniMax Code",
        bins: &["mcode"],
        local: Some("{bin} acp"),
        registry: Some("minimax-code"),
        setup: "Install MiniMax Code (npm install -g @minimax-ai/code) and run `mcode login`.",
        install: Some(Install::Npm("@minimax-ai/code")),
        login: "{bin} login",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "junie",
        aliases: &[],
        name: "Junie",
        bins: &["junie"],
        local: Some("{bin} --acp=true"),
        registry: Some("junie"),
        setup: "Install Junie (curl -fsSL https://junie.jetbrains.com/install.sh | bash) and sign in with `junie`.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://junie.jetbrains.com/install.sh | bash",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"iex (irm 'https://junie.jetbrains.com/install.ps1')\"",
        }),
        login: "{bin}",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "antigravity",
        aliases: &["agy"],
        name: "Google Antigravity",
        bins: &["agy"],
        local: None,
        registry: Some("antigravity-acp"),
        setup: "Install the Antigravity CLI (curl -fsSL https://antigravity.google/cli/install.sh | bash) and sign in with `agy`.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://antigravity.google/cli/install.sh | bash",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://antigravity.google/cli/install.ps1 | iex\"",
        }),
        login: "{bin}",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces {
            tool_hooks: false,
            permission_modes: true,
            per_run_mcp: false,
            role_prompt: RolePrompt::FirstMessage,
            interactive: true,
        },
    },
    Engine {
        id: "cortex",
        aliases: &[],
        name: "Cortex Code",
        bins: &["cortex"],
        local: Some("{bin} acp serve"),
        registry: Some("cortex-code"),
        setup: "Install Cortex Code (curl -LsS https://ai.snowflake.com/static/cc-scripts/install.sh | sh) and set up a connection with `cortex`.",
        install: Some(Install::Scripts {
            unix: "curl -LsS https://ai.snowflake.com/static/cc-scripts/install.sh | sh",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://ai.snowflake.com/static/cc-scripts/install.ps1 | iex\"",
        }),
        login: "{bin}",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    Engine {
        id: "poolside",
        aliases: &[],
        name: "Poolside",
        bins: &["pool"],
        local: Some("{bin} acp"),
        registry: Some("poolside"),
        setup: "Install Poolside (curl -fsSL https://downloads.poolside.ai/pool/install.sh | sh) and run `pool login`.",
        install: Some(Install::Scripts {
            unix: "curl -fsSL https://downloads.poolside.ai/pool/install.sh | sh",
            windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://downloads.poolside.ai/pool/install.ps1 | iex\"",
        }),
        login: "{bin} login",
        signed_in: None,
        models: Models::Acp,
        oneshot: Oneshot::Acp,
        env: &[],
        enforces: Enforces::NONE,
    },
    // The kernel's own engines (kernel/adapters/harness.py) that have no ACP mode we know of: one-shot runs go through
    // their command, and the host does not list them.
    Engine {
        id: "hermes",
        aliases: &[],
        name: "Hermes Agent",
        bins: &["hermes"],
        local: None,
        registry: None,
        setup: "Install Hermes Agent and run `hermes` to set it up.",
        install: None,
        login: "",
        signed_in: None,
        models: Models::None,
        oneshot: Oneshot::Native("{bin} chat -Q --format stream-json -q {task}"),
        env: &[],
        enforces: Enforces {
            tool_hooks: false,
            permission_modes: false,
            per_run_mcp: false,
            role_prompt: RolePrompt::Env,
            interactive: true,
        },
    },
    Engine {
        id: "dsh",
        aliases: &[],
        name: "DeepSeek Harness",
        bins: &["dsh"],
        local: None,
        registry: None,
        setup: "Install DeepSeek Harness (npm install -g @deepseek-ai/dsh) and set up a profile with `dsh`.",
        install: Some(Install::Npm("@deepseek-ai/dsh")),
        login: "",
        signed_in: None,
        models: Models::None,
        oneshot: Oneshot::Native("{bin} --profile headless --json -"),
        env: &["DEEPSEEK_API_KEY"],
        enforces: Enforces {
            tool_hooks: false,
            permission_modes: false,
            per_run_mcp: false,
            role_prompt: RolePrompt::FirstMessage,
            interactive: false,
        },
    },
];

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::collections::HashSet;

    fn nothing(_: &str) -> Option<PathBuf> {
        None
    }

    #[test]
    fn ids_and_aliases_are_unique() {
        let mut seen = HashSet::new();
        for e in ENGINES {
            assert!(seen.insert(e.id), "{} twice", e.id);
            for a in e.aliases {
                assert!(seen.insert(a), "alias {a} twice");
            }
        }
    }

    #[test]
    fn alias_finds_the_row() {
        assert_eq!(find("agy").map(|e| e.id), Some("antigravity"));
        assert_eq!(find("claude-code").map(|e| e.id), Some("claude"));
        assert_eq!(find("codex").map(|e| e.id), Some("codex"));
        assert!(find("nope").is_none());
        // A stored backend id is matched by id only, as the host always did.
        assert!(by_id("agy").is_none());
        assert!(by_id("antigravity").is_some());
    }

    #[test]
    fn every_kernel_harness_has_a_row() {
        // kernel/adapters/harness.py HARNESSES, and claude.
        for id in [
            "claude", "codex", "pi", "agy", "hermes", "qwen", "dsh", "opencode",
        ] {
            assert!(find(id).is_some(), "{id} has no row");
        }
    }

    /// The kernel's own list (`HARNESSES = (...)` in kernel/adapters/harness.py) read from the file, so a harness added
    /// there without a row here fails.
    #[test]
    fn harness_py_harnesses_all_resolve() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../kernel/adapters/harness.py"
        );
        let Ok(src) = std::fs::read_to_string(path) else {
            return;
        }; // a crate packaged without the kernel
        let line = src
            .lines()
            .find(|l| l.starts_with("HARNESSES = ("))
            .expect("harness.py has a HARNESSES line");
        let ids: Vec<&str> = line.split('\'').skip(1).step_by(2).collect();
        assert!(ids.len() >= 7, "parsed {ids:?}");
        for id in ids {
            assert!(
                find(id).is_some(),
                "harness.py names {id}, which has no row in the engines table"
            );
        }
    }

    #[test]
    fn host_rows_are_the_acp_ones() {
        assert_eq!(ENGINES.iter().filter(|e| e.has_acp()).count(), 27);
        assert!(!find("hermes").unwrap().has_acp());
        assert!(!find("dsh").unwrap().has_acp());
    }

    #[test]
    fn only_claude_enforces_hooks() {
        let hooked: Vec<_> = ENGINES
            .iter()
            .filter(|e| e.enforces.tool_hooks)
            .map(|e| e.id)
            .collect();
        assert_eq!(hooked, ["claude"]);
    }

    #[test]
    fn detection_uses_the_injected_lookup_and_alias_binaries() {
        let cursor = find("cursor").unwrap();
        assert!(detect(cursor, &nothing).is_none());
        // Installed under its second program name.
        let which = |b: &str| (b == "agent").then(|| PathBuf::from("/x/agent"));
        assert_eq!(
            detect(cursor, &which),
            Some(("agent", PathBuf::from("/x/agent")))
        );
        // The first name wins when both exist.
        let both = |b: &str| Some(PathBuf::from(format!("/x/{b}")));
        assert_eq!(detect(cursor, &both).map(|(b, _)| b), Some("cursor-agent"));
    }

    #[test]
    fn describe_lists_state_without_running_anything() {
        let which = |b: &str| (b == "claude").then(|| PathBuf::from("/x/claude"));
        let v: Value = serde_json::from_str(&list_json(&which)).unwrap();
        let rows = v["engines"].as_array().unwrap();
        assert_eq!(rows.len(), ENGINES.len());
        let claude = rows.iter().find(|r| r["id"] == "claude").unwrap();
        assert_eq!(claude["installed"], true);
        assert_eq!(claude["oneshot"], "native");
        assert_eq!(claude["enforces"]["tool_hooks"], true);
        assert!(claude["signed_in"].is_null());
        let agy = rows.iter().find(|r| r["id"] == "antigravity").unwrap();
        assert_eq!(agy["installed"], false);
        assert_eq!(agy["aliases"], json!(["agy"]));
    }

    #[test]
    fn sign_in_predicates() {
        assert!(Predicate::JsonTrue("loggedIn").eval(true, r#"{"loggedIn":true}"#));
        assert!(!Predicate::JsonTrue("loggedIn").eval(true, r#"{"loggedIn":false}"#));
        assert!(!Predicate::JsonTrue("loggedIn").eval(true, "not json"));
        assert!(Predicate::SaysLoggedIn.eval(false, "Logged in as x"));
        assert!(!Predicate::SaysLoggedIn.eval(true, "Not logged in"));
        assert!(Predicate::ExitOk.eval(true, ""));
        assert!(!Predicate::ExitOk.eval(false, "ok"));
    }

    #[test]
    fn install_and_login_commands() {
        let claude = find("claude").unwrap();
        assert!(install_text(claude, true).unwrap().starts_with("curl"));
        assert!(
            install_text(claude, false)
                .unwrap()
                .starts_with("powershell")
        );
        // A Unix-only script has nothing on Windows.
        assert!(install_text(find("opencode").unwrap(), false).is_err());
        assert_eq!(
            install_text(find("gemini").unwrap(), true).unwrap(),
            "npm install -g @google/gemini-cli"
        );
        assert_eq!(login_text(claude, &nothing).unwrap(), "claude auth login");
        let which = |b: &str| (b == "agent").then(|| PathBuf::from("/x/agent"));
        assert_eq!(
            login_text(find("cursor").unwrap(), &which).unwrap(),
            "NO_OPEN_BROWSER=1 agent login"
        );
        assert!(login_text(find("hermes").unwrap(), &nothing).is_err());
    }
}
