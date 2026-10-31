"""What a run for anyone but the owner may not touch: the secrets and the run-later paths (rule W).

One list, read by both layers `kernel/bin/kith` writes for such a run: the OS sandbox around Bash (denyRead / denyWrite)
and the permission rules of the file tools (Read / Edit deny rules, which beat any allow). Keep `serve/host/src/cards/
resource.rs` SENSITIVE in step with SECRET_RULES.

Rule W (designs/2026-10-24-cards-plan.md, plan 2): a run that is not the owner's never writes a file that something
outside the sandbox later runs, loads as settings, or loads as instructions. Everything here is by real path and, where
a link is involved, by both spellings.
"""
import os
import re

# Under the Kith state folder and the machine folder: whole folders that hold logins, tokens and every audience's index.
SECRET_DIRS = ("secrets", "serve", "browsers", "bridge", "index", "room-ledger")
# The same places as permission-rule patterns (Read/Edit), plus the database files that sit among them.
SECRET_RULES = ("secrets/**", "serve/token", "serve/identity.json", "serve/*.db", "serve/*.db-*", "browsers/**", "bridge/**",
                "index/**", "memory.db*")

# Below any folder a run may write in (and below the working directory, which is read-only for a guest): files and
# folders that Claude Code, git, an editor or a shell loads and runs on the owner's next call.
IN_FOLDER_TREES = (".claude", ".vscode", ".idea", ".git/hooks")
IN_FOLDER_FILES = ("CLAUDE.md", "CLAUDE.local.md", ".mcp.json", ".envrc", ".git/config", ".gitmodules")
# Below the owner's home folder.
HOME_TREES = (".config/git", "Library/LaunchAgents", ".local/bin", ".npm-global", ".cargo/bin")
HOME_FILES = (".zshrc", ".zshenv", ".zprofile", ".zlogin", ".bashrc", ".bash_profile", ".profile", ".gitconfig", ".claude.json")
# Absolute: command folders on PATH.
ABS_TREES = ("/opt/homebrew", "/usr/local/bin")


def both(p):
    return list(dict.fromkeys((p, os.path.realpath(p))))


def secret_paths(roots, secrets_dir, machine):
    """Absolute folders (denyRead / denyWrite) for the secrets: `roots` are the state and machine folders."""
    out = []
    for r in dict.fromkeys(x for root in roots for x in both(root)):
        out += ["%s/%s" % (r, f) for f in SECRET_DIRS]
    machine = both(machine)
    for p in both(secrets_dir):   # the older layout keeps two files among the machine things
        out += [p + "/env", p + "/auth.json"] if p in machine else [p]
    return list(dict.fromkeys(out))


def whole_secret_paths(roots, secrets_dir, machine):
    """`secret_paths` for a grant of access: when the secrets folder is the machine folder itself (the shared layout, with no
    ~/.kith), the whole folder counts, not just env and auth.json: no guest needs it."""
    return list(dict.fromkeys(secret_paths(roots, secrets_dir, machine) + both(secrets_dir)))


def holds(path, inner):
    """Whether `path` is `inner` or a folder above it (a grant of `path` would reach `inner`). Case-folded on macOS."""
    if os.uname().sysname == "Darwin":
        path, inner = path.lower(), inner.lower()
    return inner == path or inner.startswith(path.rstrip("/") + "/")


def rule_w(home, folders, trees=(), files=()):
    """(trees, files) the run may not write: below each of `folders`, below `home`, the PATH folders, and the extra
    `trees` / `files` (the kernel, the agent's package, ...). Both spellings of each."""
    t, f = [], []
    for d in folders:
        for x in both(d):
            # at any depth: a repository cloned or created below a folder the run may write in
            for pre in ("%s/" % x, "%s/**/" % x):
                t += [pre + n for n in IN_FOLDER_TREES]
                f += [pre + n for n in IN_FOLDER_FILES]
    for x in both(home):
        t += ["%s/%s" % (x, n) for n in HOME_TREES]
        f += ["%s/%s" % (x, n) for n in HOME_FILES]
    for a in ABS_TREES:
        t += both(a)
    for p in trees:
        t += both(p)
    for p in files:
        f += both(p)
    return list(dict.fromkeys(t)), list(dict.fromkeys(f))


def edit_rules(trees, files):
    """Edit deny rules (which cover every tool that writes a file) for a rule W list."""
    return ["Edit(/%s/**)" % t for t in trees] + ["Edit(/%s)" % f for f in files]


# ---- access grants (F11 step 2b)
# What a yes on an escape card adds to the next run's profile. The host decides what is granted (cards, kith-auth); this is
# the build-time half, and it checks again: a grant is never added when it resolves into the secrets or rule W, however
# it got here.
DOMAIN = re.compile(r"^(?=.{1,253}$)([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]([a-z0-9-]{0,61}[a-z0-9])?$")


def valid_domain(d):
    """A plain host name (no wildcard, no address, no port)."""
    return isinstance(d, str) and DOMAIN.match(d) is not None


def covers(entry, path):
    """Whether a deny entry (a path, `dir/**` for a tree, `dir/**/name` at any depth) holds `path`. Case-folded on macOS."""
    if os.uname().sysname == "Darwin":
        entry, path = entry.lower(), path.lower()
    e = entry[:-3] if entry.endswith("/**") else entry
    rx = re.escape(e).replace("/\\*\\*/", "/(?:.*/)?")
    return re.fullmatch(rx + "(?:/.*)?", path) is not None


def is_rule_w(home, path, trees=(), files=()):
    """Whether `path` is, or is below, a place rule W closes: the in-folder names at any depth, the home and PATH folders,
    and the extra `trees` / `files`."""
    t, f = rule_w(home, [], trees, files)
    t += ["/**/" + n for n in IN_FOLDER_TREES]
    f += ["/**/" + n for n in IN_FOLDER_FILES]
    return any(covers(x, path) for x in t) or any(covers(x, path) for x in f)


def apply_grants(s, grants, home, secrets):
    """Adds `grants` ({"read": [...], "write": [...], "domains": [...]}) to the guest settings `s` (a dict, in place).

    `secrets` is the list of absolute secret folders (`secret_paths`). Returns the (kind, value) pairs that went in; the
    rest are dropped: not an absolute real path (a link on the way now leads elsewhere, so the grant is for something
    else), inside a secret or a denied place, inside rule W for a write, or a bad domain name."""
    fs = s["sandbox"]["filesystem"]
    perms = s.setdefault("permissions", {})
    deny = perms.setdefault("deny", [])
    allow = perms.setdefault("allow", [])
    add_dirs = perms.setdefault("additionalDirectories", [])
    # Named denies (other agents, other people, the owner's user.md) and the secrets stay on top of everything below. The
    # Kith folder and agents folder are denied whole with a way back in, so a grant inside them is judged by the named ones.
    whole = {p for r in ("KITH_HOME", "KITH_AGENTS") if os.environ.get(r) for p in both(os.path.abspath(os.environ[r]))}
    never_read = [d for d in fs.get("denyRead", []) if d != "/" and d not in whole] + list(secrets)
    never_write = list(fs.get("denyWrite", [])) + list(secrets)
    done = []

    def put(lst, x):
        if x not in lst:
            lst.append(x)

    def real(p):
        return p if isinstance(p, str) and os.path.isabs(p) and os.path.realpath(p) == p else None

    writes = {p for p in grants.get("write", []) if real(p)}
    for kind in ("read", "write"):
        for p in dict.fromkeys(grants.get(kind, [])):
            p = real(p)
            if not p or any(covers(d, p) for d in never_read) or any(holds(p, d) for d in secrets):
                continue
            if kind == "write":
                if any(covers(d, p) for d in never_write):
                    continue
                if is_rule_w(home, p):
                    continue
                trees, files = rule_w(home, [p])
                for t in trees:
                    put(fs.setdefault("denyWrite", []), t)
                    if "**" in t:
                        put(fs["denyWrite"], t + "/**")
                for f in files:
                    put(fs.setdefault("denyWrite", []), f)
                for r in edit_rules(trees, files):
                    put(deny, r)
                put(fs.setdefault("allowWrite", []), p)
                put(allow, "Edit(/%s/**)" % p if os.path.isdir(p) else "Edit(/%s)" % p)
            elif p not in writes:
                # read only: the file tools may not write there, whatever the permission mode would allow
                put(deny, "Edit(/%s/**)" % p if os.path.isdir(p) else "Edit(/%s)" % p)
            put(fs.setdefault("allowRead", []), p)
            if os.path.isdir(p):
                put(add_dirs, p)
            else:
                put(allow, "Read(/%s)" % p)
            done.append((kind, p))
    for d in dict.fromkeys(grants.get("domains", [])):
        if valid_domain(d):
            put(s["sandbox"].setdefault("network", {}).setdefault("allowedDomains", []), d)
            done.append(("domain", d))
    return done


# ---- must ask (designs/2026-11-01-routine-vs-ask): five classes that no allow outranks
# Routine is what an agent's package declares (settings.json permissions.allow, autoMode.allow). These classes win over
# that and over every "Always allow": `must_ask` judges one call (kith_allowed.derive, kith-pkg check), `must_ask_rules`
# writes them as permission rules for every run (bin/kith, the settings fill): ask (deny for secrets), which Claude Code
# puts before any allow rule and any hook allow, in every mode, bypassPermissions too.
#   secret   a credential or a login
#   trust    a file that changes what later runs may do: allow lists, settings, capabilities, run/once, the agent's own
#            package, the repo's protected paths (.github/kith-protected-paths), shell start-up files, LaunchAgents, PATH
#   destroy  a delete or overwrite outside the run's writable roots
#   outward  public or past the owner: a force push or a push to main, a merge, a release, a publish or deploy, a network
#            post, a message to anyone but the owner, spending
#   code     code with no name: an interpreter or shell given inline code, a package runner, eval, a pipe into a shell
CLASSES = ("secret", "trust", "destroy", "outward", "code")

SECRET_WORDS = re.compile(
    r'(?i)\.ssh\b|\.aws\b|\.gnupg\b|\.netrc|\.npmrc|\.pypirc|\.docker/config|\bid_(?:rsa|dsa|ed25519|ecdsa)\b|auth\.json'
    r'|serve/token|\.kith/(?:secrets|serve|browsers|bridge|index)\b|/secrets/|\bkeychain|\bsecurity\s+(?:find|dump|add|delete)-'
    r'|(?<![A-Za-z])(?:credentials?|passwords?|passwd|api[_-]?keys?|secrets?|tokens?|bearer|private[_-]?key)(?![a-z])|\.pem\b|\.env\b')
# Under the home folder: other tools' logins.
HOME_SECRETS = (".ssh/id_*", ".aws/credentials", ".netrc", ".npmrc", ".pypirc", ".docker/config.json", ".config/gh/hosts.yml",
                ".gnupg/**")

# Commands, as their leading words. `kith-auth` also answers as hq-auth and `kith auth`; each is also matched with a path.
SECRET_CMDS = ("security find-generic-password", "security find-internet-password", "security dump-keychain",
               "security add-generic-password", "security add-internet-password", "security delete-generic-password",
               "security delete-internet-password", "gh auth login", "gh auth token", "gh auth refresh",
               "gcloud auth login", "gcloud auth print-access-token", "gcloud auth print-identity-token",
               "gcloud auth application-default", "npm login", "npm adduser", "npm token", "docker login",
               "aws configure", "wrangler login", "kith-auth set", "kith-auth get", "kith-auth once")
TRUST_CMDS = ("kith allow", "kith-allow", "kith-auth grant", "kith-auth revoke", "kith-auth person", "kith-auth member",
              "kith-auth bot", "kith-auth forget", "kith-auth access", "launchctl load", "launchctl bootstrap")
DESTROY_CMDS = ("shred", "srm", "mkfs", "diskutil erase", "diskutil partitionDisk", "diskutil zeroDisk",
                "diskutil secureErase", "diskutil apfs delete", "git reset --hard", "git clean")
OUTWARD_CMDS = ("gh pr merge", "gh repo delete", "gh release create", "gh release delete", "gh release upload",
                "gh release edit", "gh gist create", "npm publish", "pnpm publish", "yarn publish", "yarn npm publish",
                "cargo publish", "uv publish", "poetry publish", "twine upload", "gem push", "docker push",
                "wrangler deploy", "wrangler publish", "netlify deploy", "vercel deploy", "fly deploy", "sendmail", "mail")
CODE_CMDS = ("npx", "pnpx", "bunx", "uvx", "pnpm dlx", "yarn dlx", "npm exec", "npm x", "bun x", "pipx run", "uv tool run",
             "eval", "deno eval")
# Interpreters and the flags that hand them code inline.
INLINE = {"python": ("-c",), "node": ("-e", "--eval", "-p", "--print"), "deno": ("eval",), "bun": ("-e", "--eval", "eval"),
          "ruby": ("-e",), "perl": ("-e", "-E"), "php": ("-r",), "osascript": ("-e",),
          "bash": ("-c", "-s"), "sh": ("-c", "-s"), "zsh": ("-c", "-s"), "dash": ("-c", "-s"), "fish": ("-c",)}
SHELLS = ("bash", "sh", "zsh", "dash", "fish")
# Commands that run the command after them (uv run python -c ..., find -exec sh -c ...).
LAUNCHERS = {"uv", "poetry", "pipenv", "pdm", "hatch", "conda", "rye", "pyenv", "mise", "asdf", "xcrun", "nix", "arch",
             "xargs", "find", "npm", "pnpm", "yarn", "bun"}
# Words in front of a command that run it (the command is what is judged).
WRAPPERS = {"env", "command", "builtin", "nohup", "time", "nice", "exec", "sudo", "doas", "caffeinate"}
WRITERS = {"tee", "touch", "chmod", "chown", "truncate", "install"}
LAST_WRITERS = {"cp", "mv", "ln", "rsync", "ditto"}
DELETERS = {"rm", "rmdir", "unlink", "trash", "truncate"}
REDIRECTS = (">", ">>", ">|", "&>", "&>>")

# One sentence per class for the auto-mode classifier (autoMode.soft_deny), for what the rules cannot spell.
SOFT_DENY = {
    "secret": "Never read, print or send a password, token, key or login (Keychain, kith-auth values, ~/.ssh, cloud logins).",
    "trust": "Never change a file that decides what later runs may do (any allowed.json, settings.json, capabilities.yaml, "
             "run/once, the agent's own package outside skills/learned, kith's protected paths, shell start-up files, "
             "LaunchAgents, folders on PATH) without the owner's yes.",
    "destroy": "Never delete or overwrite anything outside the run's own writable folders and its working copy (rm -rf, "
               "git reset --hard, git clean, a disk tool) without the owner's yes.",
    "outward": "Never do something public or past the owner without the owner's yes: a force push or a push to main or "
               "master, a merge, a release, a publish or deploy, a network post, a message to anyone but the owner, "
               "spending money.",
    "code": "Never run code that has no name: an interpreter or shell given inline code, a package fetched and run "
            "(npx, uvx, dlx), eval, or a download piped into a shell.",
}
# What a sentence for autoMode.allow may not name (kith-pkg check).
SENTENCE = {
    "secret": re.compile(r"(?i)\b(passwords?|tokens?|credentials?|keychain|api[ _-]?keys?|secrets?|logins?|sign[ -]?in)\b"),
    "trust": re.compile(r"(?i)allowed\.json|settings(?:\.local)?\.json|capabilities\.yaml|run/once|\.zshrc|\.bashrc|"
                        r"\.profile|launchagents|protected[ -]paths|\bkith[ -]allow\b"),
    "destroy": re.compile(r"(?i)\brm\s+-\w*r|\bdelet\w*|\bwipe|\berase|reset --hard|git clean|\boverwrit"),
    "outward": re.compile(r"(?i)\b(push\w*|publish\w*|merg\w*|deploy\w*|releas\w*|post\w* to|send\w* (to|an? e-?mail)|"
                          r"e-?mail\w*|tweet\w*|pay\w*|purchas\w*|buy\w*|spend\w*)\b"),
    "code": re.compile(r"(?i)\b(npx|uvx|bunx|pnpx|dlx|eval|inline code|python3? -c|node -e|ba?sh -c|curl[^.]*\|\s*\w*sh)\b"),
}


def _expand(p, home):
    if p == "~" or p.startswith("~/"):
        return home + p[1:]
    for v in ("$HOME", "${HOME}"):
        if p == v or p.startswith(v + "/"):
            return home + p[len(v):]
    return p


class Places:
    """Where the trust and secret paths are for one run (or one package): from kith_paths unless given."""

    def __init__(self, package=None, home=None, state=None, machine=None, secrets=None, agents=None, kernel=None):
        import kith_paths
        p = kith_paths.paths()
        self.home = str(home or os.path.expanduser("~"))
        self.state = both(str(state or p["KITH_STATE"]))
        self.machine = both(str(machine or p["KITH_MACHINE"]))
        self.secrets_dir = str(secrets or p["KITH_SECRETS"])
        roots = [str(agents or p["KITH_AGENTS"])] + [s + "/agents" for s in self.state]
        self.agents = list(dict.fromkeys(x for r in roots for x in both(r)))
        kernel = str(kernel or kith_paths.KERNEL)
        repos = [str(p["KITH_HOME"])] + ([os.path.dirname(kernel)] if os.path.basename(kernel) == "kernel" else [])
        self.repos = list(dict.fromkeys(x for r in repos for x in both(r)
                                        if os.path.isfile(os.path.join(r, ".github/kith-protected-paths"))))
        self.kernel = both(kernel)
        self.package = both(str(package)) if package else []

    def secret_trees(self):
        out = whole_secret_paths(self.state + self.machine, self.secrets_dir, self.machine[0])
        return out + ["%s/%s" % (h, s) for h in both(self.home) for s in HOME_SECRETS]

    def protected(self):
        """(trees, files) of the repo's protected paths, under each live checkout of the kith repo."""
        t, f = [], []
        for r in self.repos:
            try:
                lines = open(os.path.join(r, ".github/kith-protected-paths")).read().splitlines()
            except OSError:
                continue
            for line in lines:
                line = line.strip()
                if not line or line.startswith("#") or ".." in line or line.startswith("/"):
                    continue
                (t if line.endswith("/") else f).append(os.path.join(r, line.rstrip("/")))
        return t, f

    def trust(self):
        """(trees, files, globs): trees and files are absolute; globs are `fnmatch` patterns on an absolute path."""
        trees, files = self.protected()
        globs = []
        for a in self.agents:
            globs += [a + "/*/allowed.json", a + "/*/allowed.json.*", a + "/*/role/settings.json",
                      a + "/*/role/capabilities.yaml", a + "/*/role/mcp.json"]
        for h in both(self.home):
            files += [h + "/.claude/settings.json", h + "/.claude/settings.local.json"]
            files += ["%s/%s" % (h, n) for n in HOME_FILES]
            trees += ["%s/%s" % (h, n) for n in HOME_TREES]
        for d in self.state + self.machine:
            files.append(d + "/settings.json")
        for m in self.machine:
            trees.append(m + "/run/once")
        for a in ABS_TREES:
            trees += both(a)
        trees += self.package
        globs += ["*/.claude/settings.json", "*/.claude/settings.local.json", "*/.git/hooks/*"]
        return list(dict.fromkeys(trees)), list(dict.fromkeys(files)), globs

    def is_trust(self, path):
        import fnmatch
        path = _expand(path, self.home)
        if not path.startswith("/"):
            path = "/" + path   # a relative path: only the at-any-depth names can be told
        trees, files, globs = self.trust()
        for x in dict.fromkeys((path, os.path.realpath(path))):
            for pk in self.package:   # the agent's own learned skills are its to write
                if holds(pk + "/skills/learned", x):
                    return False
            if any(holds(t, x) for t in trees) or any(_same(f, x) for f in files):
                return True
            if any(fnmatch.fnmatch(x.lower() if _fold() else x, g.lower() if _fold() else g) for g in globs):
                return True
        return False

    def is_secret(self, path):
        import fnmatch
        path = _expand(path, self.home)
        for x in dict.fromkeys((path, os.path.realpath(path))):
            for s in self.secret_trees():
                if "*" in s:
                    if fnmatch.fnmatch(x, s.replace("/**", "/*")) or fnmatch.fnmatch(x, s.replace("/**", "")):
                        return True
                elif covers(s, x):
                    return True
        return False


def _fold():
    return os.uname().sysname == "Darwin"


def _same(a, b):
    return a.lower() == b.lower() if _fold() else a == b


def _names_secret(text, places):
    if SECRET_WORDS.search(text):
        return True
    secrets = os.environ.get("KITH_SECRETS")
    if secrets and len(secrets) > 1 and secrets in text:
        return True
    for r in places.state + places.machine:
        if any("%s/%s" % (r, d) in text for d in SECRET_DIRS):
            return True
    try:
        from kith_trajectory import redact
        return redact(text) != text
    except Exception:  # noqa: BLE001 - a missing redactor must not let a secret through
        return False


def _segments(command):
    """The simple commands of a shell line, as word lists (a quote-aware split on ; & | && || ( ) and newlines)."""
    import shlex
    lex = shlex.shlex(command.replace("\n", " ; "), posix=True, punctuation_chars=";&|()<>")
    lex.whitespace_split = True
    try:
        toks = list(lex)
    except ValueError:
        toks = command.replace("\n", " ; ").split()
    segs, cur = [], []
    for t in toks:
        if t and set(t) <= set(";&|()"):
            if cur:
                segs.append(cur)
            cur = []
        else:
            cur.append(t)
    if cur:
        segs.append(cur)
    return segs


def _strip(words):
    """The command a segment runs: leading VAR=value words and wrappers (env, nohup, sudo, timeout N...) taken off."""
    w = list(words)
    while w:
        if re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", w[0]):
            w = w[1:]
        elif os.path.basename(w[0]) in WRAPPERS:
            w = w[1:]
            while w and w[0].startswith("-"):
                w = w[1:]
        elif os.path.basename(w[0]) == "timeout" and len(w) > 1:
            w = w[2:]
        else:
            break
    return w


def _base(word):
    b = os.path.basename(word)
    return "python" if re.fullmatch(r"python(?:\d+(?:\.\d+)?)?|pypy\d*", b) else b


def _starts(words, cmds):
    """Whether the command's leading words are one of `cmds` (the first word by its name, any path; kith-auth also as hq-auth
    and `kith auth`, kith allow also as hq allow)."""
    if not words:
        return False
    first = os.path.basename(words[0])
    lead = [{"hq-auth": "kith-auth", "hq-allow": "kith-allow", "hq": "kith"}.get(first, first)] + words[1:]
    if lead[0] == "kith" and len(lead) > 1 and lead[1] == "auth":
        lead = ["kith-auth"] + lead[2:]
    if lead[0] == "git":   # git -C <dir> x: judged as git x
        while len(lead) > 2 and lead[1] in ("-C", "-c"):
            lead = [lead[0]] + lead[3:]
    for c in cmds:
        cw = c.split()
        if lead[:len(cw)] == cw or (cw[0] == "mkfs" and lead[0].startswith("mkfs")):
            return True
    return False


def _git_args(words):
    w = list(words[1:])
    while len(w) > 1 and w[0] in ("-C", "-c"):
        w = w[2:]
    return w


def _push_outward(words):
    """A force push, a push of tags, or a push to main or master."""
    w = _git_args(words)
    if not w or w[0] != "push":
        return False
    for a in w[1:]:
        if a in ("-f", "--force", "--tags", "--follow-tags", "--mirror", "--delete", "-d") or a.startswith("--force") or \
                (a.startswith("-") and not a.startswith("--") and "f" in a[1:]):
            return True
        if not a.startswith("-") and (a.startswith("+") or a.startswith(":") or "refs/tags/" in a):
            return True
        ref = a.split(":")[-1] if ":" in a else a
        if not a.startswith("-") and re.sub(r"^refs/heads/", "", ref) in ("main", "master"):
            return True
    return False


def _gh_api_write(words):
    w = words[1:]
    if w[:1] != ["api"]:
        return False
    for i, a in enumerate(w):
        if a in ("-X", "--method") and i + 1 < len(w) and w[i + 1].upper() != "GET":
            return True
        if re.fullmatch(r"(?:-X|--method=)(?!GET$)\w+", a, re.I):
            return True
        if a in ("-f", "-F", "--field", "--raw-field", "--input") or a.startswith(("--field=", "--raw-field=", "--input=")):
            return True
    return False


def _curl_write(words):
    b = os.path.basename(words[0])
    for i, a in enumerate(words[1:], 1):
        if b == "curl":
            if a in ("-X", "--request") and i + 1 < len(words) and words[i + 1].upper() not in ("GET", "HEAD"):
                return True
            if re.fullmatch(r"-X(?!GET$|HEAD$)\w+", a, re.I) or a.startswith(("--data", "--form", "--json", "--upload-file")):
                return True
            if a in ("-d", "-F", "-T") or re.fullmatch(r"-[A-Za-z]*[dFT]\S*", a) and not a.startswith("--"):
                return True
        if b == "wget" and a.startswith(("--post-data", "--post-file", "--method", "--body-data", "--body-file")):
            return True
    return False


def _inline_code(words):
    """An interpreter or shell given code inline, alone (reading stdin: a pipe into it), or fed a heredoc. Its flags are
    read up to the script it is given: `python3 tool.py -c x.yml` runs a named script."""
    starts = [0] + ([i for i in range(1, len(words)) if _base(words[i]) in INLINE]
                    if os.path.basename(words[0]) in LAUNCHERS else [])
    for i in starts:
        b = _base(words[i])
        if b not in INLINE:
            continue
        rest = words[i + 1:]
        if not rest or rest[0] in ("-", "<", "<<", "<<<"):
            return True
        for r in rest:
            if r in INLINE[b] or (b in SHELLS and re.fullmatch(r"-[a-z]*c[a-z]*", r)):
                return True
            if not r.startswith("-"):
                break
    return False


def _targets(words):
    """The files a command writes, by its spelling: redirects, tee/touch/chmod/..., the last word of cp/mv/ln, sed -i, dd of=."""
    out = []
    for i, w in enumerate(words):
        if w in REDIRECTS and i + 1 < len(words):
            out.append(words[i + 1])
    cmd = [w for w in words if w not in REDIRECTS]
    if not cmd:
        return out
    b = os.path.basename(cmd[0])
    args = [a for a in cmd[1:] if not a.startswith("-")]
    if b in WRITERS:
        out += args
    elif b in LAST_WRITERS and args:
        out.append(args[-1])
    elif b in ("sed", "gsed", "perl") and any(a.startswith("-i") or a.startswith("--in-place") for a in cmd[1:]):
        out += args[1:]
    elif b == "dd":
        out += [a[3:] for a in cmd[1:] if a.startswith("of=")]
    return out


def _inside(path, roots, home):
    path = _expand(path, home)
    if not path.startswith("/"):
        return False
    real = os.path.realpath(path)
    return any(holds(r, x) for r in roots for x in (path, real)) and real.rstrip("/") not in [r.rstrip("/") for r in roots]


def _bash(command, roots, places):
    found = set()
    for seg in _segments(command):
        words = _strip(seg)
        if not words:
            continue
        b = os.path.basename(words[0])
        if _starts(words, SECRET_CMDS):
            found.add("secret")
        if _starts(words, TRUST_CMDS):
            found.add("trust")
        if any(places.is_secret(t) for t in _targets(words)):
            found.add("secret")
        elif any(places.is_trust(t) for t in _targets(words)):
            found.add("trust")
        if _starts(words, DESTROY_CMDS) or (b == "dd" and any(a.startswith("of=") for a in words)):
            found.add("destroy")
        if b in DELETERS or (b == "find" and ("-delete" in words or "rm" in words)) or (b == "xargs" and "rm" in words):
            targets = [a for a in words[1:] if not a.startswith("-") and a not in REDIRECTS]
            if b in ("find", "xargs") or not targets or not all(_inside(t, roots, places.home) for t in targets):
                found.add("destroy")
            if b in DELETERS and any(places.is_trust(t) for t in targets):
                found.add("trust")
        if b == "git" and _push_outward(words) or b == "gh" and _gh_api_write(words) or _curl_write(words) \
                or _starts(words, OUTWARD_CMDS):
            found.add("outward")
        if _starts(words, CODE_CMDS) or _inline_code(words):
            found.add("code")
    return found


def must_ask(tool, tool_input, roots=(), places=None):
    """The must-ask class of one call ("secret", "trust", "destroy", "outward", "code"), or None for routine. `roots` are the
    folders the run may change freely (a delete inside one is routine); with none, every delete is must-ask."""
    import json
    places = places or Places()
    tool_input = tool_input if isinstance(tool_input, dict) else {}
    found = set()
    if _names_secret(json.dumps(tool_input, ensure_ascii=False), places):
        found.add("secret")
    roots = [x for r in roots for x in both(r)]
    if tool == "Bash":
        cmd = tool_input.get("command")
        if isinstance(cmd, str):
            found |= _bash(cmd, roots, places)
    elif tool in ("Edit", "Write", "MultiEdit", "NotebookEdit", "Read", "Grep", "Glob"):
        p = tool_input.get("file_path") or tool_input.get("notebook_path") or tool_input.get("path")
        if isinstance(p, str) and p:
            if places.is_secret(p):
                found.add("secret")
            elif tool not in ("Read", "Grep", "Glob") and places.is_trust(p):
                found.add("trust")
    elif tool.startswith("mcp__") and not tool.startswith("mcp__kithpermit__"):
        name = tool.rsplit("__", 1)[-1].lower()
        if re.search(r"(?:^|_)(?:merge|publish|deploy|send|post|tweet|pay|purchase|buy|transfer|invite|share)(?:_|$)", name):
            found.add("outward")
        elif re.search(r"(?:^|_)(?:delete|destroy|drop|purge|remove|wipe)(?:_|$)", name):
            found.add("destroy")
    return next((c for c in CLASSES if c in found), None)


def must_ask_rule(rule, fill=lambda s: s, places=None):
    """The must-ask class of a permission rule a package allows, as the call it lets through ("Bash(git push:*)" is
    `git push`; a wildcard word stands for any one word). None when every call it allows is routine by spelling."""
    m = re.fullmatch(r"([A-Za-z0-9_]+)(?:\((.*)\))?", rule.strip(), re.S)
    if not m:
        return must_ask(rule.strip(), {}, places=places) if rule.strip().startswith("mcp__") else None
    tool, arg = m.group(1), fill(m.group(2) or "")
    if tool.startswith("mcp__") or (not m.group(2) and tool != "Bash"):
        return must_ask(tool, {}, places=places)
    if tool == "Bash":
        prefix = arg.endswith(":*") or arg.endswith(" *")
        cmd = arg[:-2] if prefix else arg
        cmd = re.sub(r"(?:(?<=\s)|^)\*(?=\s|$)", "x", cmd).replace("*", "")
        # a prefix rule leaves the last word open: a copy's source is not where it writes
        return must_ask("Bash", {"command": cmd.strip() + (" x" if prefix else "")}, places=places)
    if tool in ("Edit", "Write", "Read", "MultiEdit", "NotebookEdit"):
        p = arg[1:] if arg.startswith("//") else arg
        p = re.sub(r"/\*\*.*$|/\*$", "", p) if p != "/" else p
        places = places or Places()
        p = _expand(p, places.home)
        if not p:
            return None
        # a rule wider than a must-ask place is fine: that place's own deny or ask rule still wins inside it
        if places.is_secret(p) or any(holds(s, p) for s in places.secret_trees() if "*" not in s):
            return "secret"
        if tool != "Read" and places.is_trust(p):
            return "trust"
    return None


def sentence_class(text):
    """The must-ask class an autoMode.allow sentence names, or None."""
    return next((c for c in CLASSES if SENTENCE[c].search(text or "")), None)


def _bash_rules(cmds, kernel=()):
    """Bash rules for command prefixes: the bare name, git's -C form, and kith's tools by their path in each kernel."""
    out = []
    for c in cmds:
        w = c.split()
        names = [w[0]]
        if w[0] == "kith-auth":
            names += ["hq-auth"]
            out += ["Bash(kith auth %s:*)" % " ".join(w[1:]), "Bash(hq auth %s:*)" % " ".join(w[1:])]
        if w[0] in ("kith-auth", "kith-allow", "kith"):
            names += ["%s/bin/%s" % (k, w[0]) for k in kernel] + ['"%s/bin/%s"' % (k, w[0]) for k in kernel]
        if w[0] == "kith" and w[1:2] == ["allow"]:
            out.append("Bash(hq allow:*)")
        for n in names:
            out.append("Bash(%s:*)" % " ".join([n] + w[1:]))
        if w[0] == "git":
            out.append("Bash(git -C * %s*)" % " ".join(w[1:]))
    return out


def must_ask_rules(places):
    """{"deny", "ask_files", "ask_bash", "soft_deny"} for one run's settings. Secrets are deny; the rest ask."""
    deny, ask_files, ask_bash = [], [], []
    for h in both(places.home):
        deny += ["Read(/%s/%s)" % (h, s) for s in HOME_SECRETS] + ["Edit(/%s/%s)" % (h, s) for s in HOME_SECRETS]
    deny += _bash_rules(SECRET_CMDS, places.kernel)
    trees, files, _ = places.trust()
    for a in places.agents:
        ask_files += ["Edit(/%s/*/role/settings.json)" % a, "Edit(/%s/*/role/capabilities.yaml)" % a,
                      "Edit(/%s/*/role/mcp.json)" % a, "Edit(/%s/*/allowed.json)" % a]
    for pk in places.package:   # the own package but skills/learned: rules cannot subtract, so every other part by name
        ask_files.append("Edit(/%s/*)" % pk)
        try:
            names = sorted(os.listdir(pk))
        except OSError:
            names = []
        for n in names:
            if n == "skills" and os.path.isdir(os.path.join(pk, n)):
                ask_files.append("Edit(/%s/skills/*)" % pk)
                for k in sorted(os.listdir(os.path.join(pk, n))):
                    if k != "learned" and os.path.isdir(os.path.join(pk, n, k)):
                        ask_files.append("Edit(/%s/skills/%s/**)" % (pk, k))
            elif os.path.isdir(os.path.join(pk, n)):
                ask_files.append("Edit(/%s/%s/**)" % (pk, n))
    ask_files += ["Edit(/%s/**)" % t for t in trees if t not in places.package] + ["Edit(/%s)" % f for f in files]
    ask_bash += _bash_rules(TRUST_CMDS, places.kernel)
    # destroy: the disk tools and git's discards; a plain rm is judged by the classifier (soft_deny), and by
    # must_ask where a call is remembered.
    ask_bash += _bash_rules(DESTROY_CMDS)
    ask_bash += ["Bash(dd *of=*)"]
    for h in ["~", "$HOME"] + both(places.home):
        ask_bash += ["Bash(rm -* %s)" % h, "Bash(rm -* %s/)" % h, "Bash(rm -* %s/*)" % h]
    ask_bash += ["Bash(rm -* /)", "Bash(rm -* /*)"]
    # outward
    for g in ("git", "git -C *"):
        for shape in ("--force*", "-f", "-f *", "* -f", "* -f *", "* --force*", "* +*", "--tags*", "* --tags*",
                      "--follow-tags*", "* --follow-tags*", "--mirror*", "* --mirror*", "* --delete *", "* -d *",
                      "* main", "* main *", "* master", "* master *", "*:main", "*:main *", "*:master",
                      "*:master *", "*:refs/heads/main*", "*:refs/heads/master*", "* refs/tags/*"):
            ask_bash.append("Bash(%s push %s)" % (g, shape))
    ask_bash += _bash_rules([c for c in OUTWARD_CMDS if c not in ("mail",)])
    ask_bash += ["Bash(mail -s *)", "Bash(mail * -s *)"]
    for m in ("POST", "PUT", "PATCH", "DELETE"):
        ask_bash += ["Bash(gh api * -X %s*)" % m, "Bash(gh api -X %s*)" % m, "Bash(gh api * --method %s*)" % m,
                     "Bash(gh api --method %s*)" % m, "Bash(curl * -X %s*)" % m, "Bash(curl -X %s*)" % m,
                     "Bash(curl * --request %s*)" % m, "Bash(curl --request %s*)" % m, "Bash(curl *-X%s*)" % m]
    for f in ("-f", "-F", "--field", "--raw-field", "--input"):
        ask_bash += ["Bash(gh api * %s *)" % f, "Bash(gh api %s *)" % f]
    for f in ("-d", "--data*", "-F", "--form*", "--json*", "-T", "--upload-file*"):
        ask_bash += ["Bash(curl * %s *)" % f.rstrip("*") if not f.endswith("*") else "Bash(curl * %s)" % f,
                     "Bash(curl %s *)" % f.rstrip("*") if not f.endswith("*") else "Bash(curl %s)" % f]
    ask_bash += ["Bash(wget * --post-*)", "Bash(wget --post-*)"]
    # code
    ask_bash += _bash_rules(CODE_CMDS)
    for b, flags in INLINE.items():
        names = [b] if b != "python" else ["python", "python3"]
        for n in names:
            ask_bash += ["Bash(%s)" % n, "Bash(%s -)" % n, "Bash(%s - *)" % n]
            ask_bash += ["Bash(%s %s *)" % (n, f) for f in flags] + ["Bash(%s %s)" % (n, f) for f in flags]
            if b in SHELLS:
                ask_bash.append("Bash(%s -*c *)" % n)
    return {"deny": list(dict.fromkeys(deny)), "ask_files": list(dict.fromkeys(ask_files)),
            "ask_bash": list(dict.fromkeys(ask_bash)), "soft_deny": [SOFT_DENY[c] for c in CLASSES]}
