#!/usr/bin/env bash
# Checks that the `windows` path filter in .github/workflows/ci.yml covers every path that builds into the crates
# scripts/ci/windows-clippy.sh clippies: the crates themselves, their path dependencies (transitively), the build
# inputs they read (Cargo.toml, build.rs, include_str!/include_bytes! targets outside the crate), the toolchain pin,
# and the scripts and workflow that define the job. A missing path means a PR touching it never runs Windows CI.
# The crate list comes from `windows-clippy.sh --list-crates`, so that script stays the one source of truth.
# Plain python3 and regexes (no cargo), so it runs anywhere in seconds. Usage: scripts/ci/windows-filter-check.sh
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
crates="$(bash "$root/scripts/ci/windows-clippy.sh" --list-crates)"
exec python3 - "$root" $crates <<'PY'
import os, re, sys

root, wanted = sys.argv[1], sys.argv[2:]
os.chdir(root)

# Every package in the repo: name -> dir, and its path dependencies (any table: dependencies, dev, build, target).
pkgs = {}
for base, dirs, files in os.walk("."):
    dirs[:] = [d for d in dirs if d not in ("target", "target-xwin", "node_modules", ".git", ".claude")]
    if "Cargo.toml" not in files:
        continue
    text = open(os.path.join(base, "Cargo.toml")).read()
    m = re.search(r'^\[package\][^\[]*?^name\s*=\s*"([^"]+)"', text, re.M | re.S)
    if m:
        deps = [os.path.normpath(os.path.join(base, p)) for p in re.findall(r'path\s*=\s*"([^"]+)"', text)]
        pkgs[m.group(1)] = (os.path.normpath(base), deps)
dir_of = {d: n for n, (d, _) in pkgs.items()}

errors = []
closure = {}
todo = []
for name in wanted:
    if name not in pkgs:
        errors.append(f"windows-clippy.sh lists crate {name}, but no Cargo.toml in the repo has that package name")
    else:
        todo.append(pkgs[name][0])
while todo:
    d = todo.pop()
    if d in closure or d not in dir_of:  # a path such as src/main.rs is not a package dir
        continue
    closure[d] = dir_of[d]
    todo.extend(pkgs[dir_of[d]][1])

required = {}  # sample path -> why
def need(path, why):
    required.setdefault(path, why)

for d, name in sorted(closure.items()):
    d = d.lstrip("./")
    need(f"{d}/src/x.rs", f"source of {name}")
    need(f"{d}/Cargo.toml", f"manifest of {name}")
    if os.path.exists(f"{d}/build.rs"):
        need(f"{d}/build.rs", f"build script of {name}")
    # include_str!/include_bytes! reaching outside the crate. The all-targets crates compile their tests on Windows,
    # so every file counts; the others (kith-serve --bins, path deps) only their non-test code: test files and
    # anything after a #[cfg(test)] are skipped.
    all_targets = name in wanted and name != "kith-serve"
    for base, _, files in os.walk(d):
        if "target" in base.split(os.sep):
            continue
        for f in files:
            full = os.path.join(base, f)
            if not f.endswith(".rs"):
                continue
            text = open(full).read()
            if not all_targets:
                if "tests" in full.split(os.sep) or f.startswith("tests") or f.endswith("_tests.rs"):
                    continue
                text = text.split("#[cfg(test)]")[0]
            for rel in re.findall(r'include_(?:str|bytes)!\(\s*"([^"]+)"', text):
                target = os.path.normpath(os.path.join(base, rel))
                if not target.startswith(d + os.sep) and os.path.exists(target):
                    need(target, f"included by {full} ({name})")

for p, why in [
    ("serve/host/Cargo.lock", "workspace lockfile"),
    ("serve/host/clippy.toml", "clippy config the script runs under"),
    ("serve/rust-toolchain.toml", "pinned toolchain"),
    ("scripts/ci/windows-clippy.sh", "the Windows job's clippy script"),
    ("scripts/ci/windows-filter-check.sh", "this check"),
    (".github/workflows/ci.yml", "the workflow"),
]:
    if os.path.exists(p):
        need(p, why)

# The first positive pattern of the `windows:` filter in ci.yml: one quoted brace glob.
ci = open(".github/workflows/ci.yml").read()
m = re.search(r"^ *windows:\n *- '\{([^']*)\}'", ci, re.M)
if not m:
    print("windows-filter-check: no `windows:` filter with a quoted {a,b,...} glob found in .github/workflows/ci.yml", file=sys.stderr)
    sys.exit(2)
globs = [g.strip() for g in m.group(1).split(",") if g.strip()]

def rx(g):
    out, i = "", 0
    while i < len(g):
        if g.startswith("**/", i): out += "(?:.*/)?"; i += 3
        elif g.startswith("**", i): out += ".*"; i += 2
        elif g[i] == "*": out += "[^/]*"; i += 1
        elif g[i] == "?": out += "[^/]"; i += 1
        else: out += re.escape(g[i]); i += 1
    return re.compile("^" + out + "$")
regs = [rx(g) for g in globs]

for path, why in sorted(required.items()):
    if not any(r.match(path) for r in regs):
        errors.append(f"not covered by the windows filter: {path} ({why})")

if errors:
    for e in errors:
        print(f"::error title=windows filter::{e}", file=sys.stderr)
    print("Add the missing paths to the `windows:` filter (second `filters: |` block) in .github/workflows/ci.yml.", file=sys.stderr)
    sys.exit(1)
print(f"windows filter covers {len(required)} paths from {len(closure)} crates: {', '.join(sorted(closure.values()))}")
PY
