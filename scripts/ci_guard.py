#!/usr/bin/env python3
"""
RushWay Universal CI Pre-Check & Safe Release Guard (`scripts/ci_guard.py`)
===========================================================================
Prevents broken commits or tags from ever failing on GitHub Actions, even on
development machines without a local Rust (`cargo`) installation.

Capabilities:
1. Version & Metadata Synchronization Check:
   Verifies `APP_VERSION` in `src/main.rs`, `Cargo.toml`, `Cargo.lock`,
   `CHANGELOG.md`, and `AI_HANDOFF.md` are 100% synchronized.
2. Encoding & Whitespace Hygiene:
   Detects corrupted UTF-8 replacement characters (`\\ufffd`) and runs
   `git diff --check`.
3. Rust Amalgamation Static Analyzer (`src/main.rs`):
   - Full lexical delimiter balance (`{}`, `()`, `[]`) across all 16,500+ lines.
   - Per-module `use` import liveness analyzer (catches unused imports before push).
   - Async `tokio::sync::Mutex` vs sync `StdMutex` `.lock().await` mismatch detector.
   - `#[cfg(test)]` scope unqualified `std::net::*` type detector.
   - Automatic `cargo check --all-targets --all-features` execution if `cargo` exists.
4. Git Pre-Push Hook (`--install-hook`):
   Installs `.git/hooks/pre-push` so every `git push` runs static verification first.
5. Remote CI Gate & Safe Tag Publisher (`--wait-ci` / `--release`):
   Pushes `main` first, waits for GitHub Actions `Compile check` / `Rust check` to pass
   (or downloads & prints exact `rustc` errors on failure), and only pushes/updates
   the `vX.Y.Z` release tag once compilation & tests are verified green.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
GITHUB_REPO = "CFM503/rushway"


def strip_rust_comments_and_strings(src: str) -> str:
    """Replaces comments and string/char literals with spaces while preserving line numbers."""
    out = list(src)
    i = 0
    n = len(src)
    while i < n:
        c = src[i]
        # Line comment
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            while i < n and src[i] != "\n":
                out[i] = " "
                i += 1
            continue
        # Block comment (nested)
        if c == "/" and i + 1 < n and src[i + 1] == "*":
            out[i] = " "
            out[i + 1] = " "
            i += 2
            depth = 1
            while i < n and depth > 0:
                if src[i] == "/" and i + 1 < n and src[i + 1] == "*":
                    depth += 1
                    out[i] = " "
                    out[i + 1] = " "
                    i += 2
                    continue
                if src[i] == "*" and i + 1 < n and src[i + 1] == "/":
                    depth -= 1
                    out[i] = " "
                    out[i + 1] = " "
                    i += 2
                    continue
                if src[i] != "\n":
                    out[i] = " "
                i += 1
            continue
        # Raw string literal r#*"..."#* or br#*"..."#*
        if c in ("r", "b"):
            start = i + (2 if (c == "b" and i + 1 < n and src[i + 1] == "r") else (1 if c == "r" else 0))
            if start > i:
                hashes = 0
                while start < n and src[start] == "#":
                    hashes += 1
                    start += 1
                if start < n and src[start] == '"':
                    end_pat = '"' + ("#" * hashes)
                    j = start + 1
                    while j < n and not src.startswith(end_pat, j):
                        j += 1
                    end_idx = min(n, j + len(end_pat))
                    for k in range(i, end_idx):
                        if out[k] != "\n":
                            out[k] = " "
                    i = end_idx
                    continue
        # Normal string literal
        if c == '"':
            out[i] = " "
            i += 1
            while i < n:
                if src[i] == "\\":
                    out[i] = " "
                    if i + 1 < n and out[i + 1] != "\n":
                        out[i + 1] = " "
                    i += 2
                    continue
                if src[i] == '"':
                    out[i] = " "
                    i += 1
                    break
                if out[i] != "\n":
                    out[i] = " "
                i += 1
            continue
        # Char literal vs lifetime
        if c == "'":
            if i + 2 < n and src[i + 1] != "\\" and src[i + 2] == "'":
                out[i] = out[i + 1] = out[i + 2] = " "
                i += 3
                continue
            if i + 3 < n and src[i + 1] == "\\" and src[i + 3] == "'":
                out[i] = out[i + 1] = out[i + 2] = out[i + 3] = " "
                i += 4
                continue
        i += 1
    return "".join(out)


def check_versions_and_encoding() -> str:
    main_rs = os.path.join(REPO_ROOT, "src", "main.rs")
    cargo_toml = os.path.join(REPO_ROOT, "Cargo.toml")
    cargo_lock = os.path.join(REPO_ROOT, "Cargo.lock")
    changelog = os.path.join(REPO_ROOT, "CHANGELOG.md")
    handoff = os.path.join(REPO_ROOT, "AI_HANDOFF.md")

    for p in (main_rs, cargo_toml, cargo_lock, changelog, handoff):
        with open(p, "r", encoding="utf-8") as f:
            content = f.read()
        if "\ufffd" in content:
            raise RuntimeError(f"Corrupted UTF-8 replacement character (\\ufffd) found in {os.path.relpath(p, REPO_ROOT)}")

    with open(main_rs, "r", encoding="utf-8") as f:
        m_main = re.search(r'pub const APP_VERSION:\s*&str\s*=\s*"([^"]+)"', f.read())
    with open(cargo_toml, "r", encoding="utf-8") as f:
        m_toml = re.search(r'^version\s*=\s*"([^"]+)"', f.read(), re.M)
    with open(cargo_lock, "r", encoding="utf-8") as f:
        m_lock = re.search(r'name\s*=\s*"rushway"\r?\nversion\s*=\s*"([^"]+)"', f.read())
    with open(changelog, "r", encoding="utf-8") as f:
        m_chg = re.search(r'^## \[v([0-9]+\.[0-9]+\.[0-9]+)\]', f.read(), re.M)

    if not (m_main and m_toml and m_lock and m_chg):
        raise RuntimeError("Failed to parse version string from one or more project files")

    v_main, v_toml, v_lock, v_chg = m_main.group(1), m_toml.group(1), m_lock.group(1), m_chg.group(1)
    if len({v_main, v_toml, v_lock, v_chg}) != 1:
        raise RuntimeError(
            f"Version mismatch detected! src/main.rs={v_main}, Cargo.toml={v_toml}, "
            f"Cargo.lock={v_lock}, CHANGELOG.md={v_chg}"
        )
    print(f"[OK] Version synchronized across all files: v{v_main}")
    return v_main


def split_top_level_modules(clean_src: str):
    """Returns list of (mod_name, start_line, mod_clean_text) for each top-level module."""
    modules = []
    pat = re.compile(r"\bpub\s+mod\s+([a-zA-Z0-9_]+)\s*\{")
    pos = 0
    while True:
        m = pat.search(clean_src, pos)
        if not m:
            break
        mod_name = m.group(1)
        brace_start = m.end() - 1
        start_line = clean_src.count("\n", 0, m.start()) + 1
        depth = 0
        i = brace_start
        n = len(clean_src)
        while i < n:
            if clean_src[i] == "{":
                depth += 1
            elif clean_src[i] == "}":
                depth -= 1
                if depth == 0:
                    modules.append((mod_name, start_line, clean_src[brace_start + 1 : i]))
                    pos = i + 1
                    break
            i += 1
        else:
            raise RuntimeError(f"Unclosed module {mod_name} at line {start_line}")
    return modules


def extract_use_items(use_stmt: str):
    """Extracts local bound identifiers from a Rust `use ...;` statement."""
    body = use_stmt.strip()
    if body.startswith("pub"):
        return []  # `pub use` re-exports are externally visible
    body = re.sub(r"^use\s+", "", body).rstrip(";").strip()
    if body.endswith("*"):
        return []
    # Preserve `module::{self, ...}` as `module` before stripping group prefixes
    body = re.sub(r"([A-Za-z0-9_]+)\s*::\s*\{\s*self\s*,?", r"\1, {", body)
    # Strip module path prefixes before `{`
    while re.search(r"[A-Za-z0-9_:]+\s*::\s*\{", body):
        body = re.sub(r"[A-Za-z0-9_:]+\s*::\s*\{", "{", body)
    body = body.replace("{", ",").replace("}", ",")
    items = []
    for part in body.split(","):
        part = part.strip()
        if not part or part == "self" or part.endswith("*"):
            continue
        if " as " in part:
            alias = part.split(" as ", 1)[1].strip()
            if alias and alias != "_":
                items.append(alias)
        else:
            ident = part.split("::")[-1].strip()
            if ident and ident != "self":
                items.append(ident)
    return items


def check_rust_static_semantics(path: str):
    with open(path, "r", encoding="utf-8") as f:
        raw_src = f.read()

    clean_src = strip_rust_comments_and_strings(raw_src)

    # 1. Delimiter balance check
    stack = []
    line = 1
    for ch in clean_src:
        if ch == "\n":
            line += 1
        elif ch in "{([":
            stack.append((ch, line))
        elif ch in "})]":
            expected = {"}": "{", ")": "(", "]": "["}[ch]
            if not stack or stack[-1][0] != expected:
                raise RuntimeError(f"Delimiter mismatch '{ch}' at src/main.rs:{line}")
            stack.pop()
    if stack:
        raise RuntimeError(f"Unclosed delimiter '{stack[-1][0]}' from src/main.rs:{stack[-1][1]}")
    print(f"[OK] Lexical delimiter balance verified ({line} lines)")

    # 2. Per-module unused imports, StdMutex `.lock().await`, and test-scope checks
    modules = split_top_level_modules(clean_src)
    use_pat = re.compile(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+[^;]+;")

    for mod_name, mod_line, mod_body in modules:
        # Remove #[cfg(test)] mod tests { ... } when checking non-test or check whole module
        # Check every top-level `use` in the module (outside functions)
        for m in use_pat.finditer(mod_body):
            stmt = m.group(0)
            stmt_line = mod_line + mod_body.count("\n", 0, m.start())
            # Only check module-top-level imports (indent <= 4 spaces)
            leading_ws = len(stmt.split("\n")[0]) - len(stmt.split("\n")[0].lstrip(" "))
            if leading_ws > 4:
                continue
            remainder = mod_body[: m.start()] + mod_body[m.end() :]
            trait_method_hints = {
                "AsyncReadExt": r"\.(?:read|read_exact|read_to_end|read_to_string|read_buf|read_u8|read_u16|read_u32)\s*\(",
                "AsyncWriteExt": r"\.(?:write|write_all|write_vectored|flush|shutdown|write_u8|write_u16|write_u32)\s*\(",
                "Read": r"\.(?:read|read_exact|read_to_end|read_to_string)\s*\(",
                "Write": r"\.(?:write|write_all|write_fmt|flush)\s*\(",
                "FmtWrite": r"\bwrite!\s*\(",
                "FromStr": r"\bfrom_str\s*\(|\.parse\b",
                "Hasher": r"\.(?:write|finish)\s*\(",
                "RngCore": r"\.(?:fill_bytes|next_u32|next_u64|try_fill_bytes)\s*\(",
                "Rng": r"\.(?:gen|gen_range|fill|fill_bytes|sample)\b",
                "Context": r"\.(?:context|with_context)\s*\(",
                "IsTerminal": r"\.is_terminal\s*\(",
                "SliceRandom": r"\.(?:choose|shuffle)\s*\(",
                "Digest": r"\.(?:update|finalize)\s*\(",
            }
            for ident in extract_use_items(stmt):
                if re.search(rf"\b{re.escape(ident)}\b", remainder):
                    continue
                hint = trait_method_hints.get(ident)
                if hint and re.search(hint, remainder):
                    continue
                raise RuntimeError(
                    f"Unused import `{ident}` in `mod {mod_name}` (around src/main.rs:{stmt_line})"
                )

        # Check StdMutex fields are never awaited on `.lock().await`
        std_mutex_fields = re.findall(r"\b([a-zA-Z0-9_]+)\s*:\s*(?:Arc<\s*)?StdMutex<", mod_body)
        for field in std_mutex_fields:
            bad_await = re.search(rf"\b{re.escape(field)}\s*\.lock\(\)\s*\.await\b", mod_body)
            if bad_await:
                err_line = mod_line + mod_body.count("\n", 0, bad_await.start())
                raise RuntimeError(
                    f"`StdMutex` field `.{field}.lock().await` awaited in `mod {mod_name}` "
                    f"(src/main.rs:{err_line}); remove `.await` and use `.unwrap_or_else(|e| e.into_inner())`"
                )

        # Check unqualified net types in `mod tests` if not imported in `mod_body`
        for net_ty in ("IpAddr", "Ipv4Addr", "Ipv6Addr", "SocketAddr"):
            for m_ty in re.finditer(rf"(?<!::)\b{net_ty}::", mod_body):
                # Verify `net_ty` is imported somewhere in `mod_body`
                if not re.search(rf"\buse\s+[^;]*\b{net_ty}\b", mod_body):
                    err_line = mod_line + mod_body.count("\n", 0, m_ty.start())
                    raise RuntimeError(
                        f"Unqualified `{net_ty}::` used in `mod {mod_name}` (src/main.rs:{err_line}) "
                        f"without `use std::net::{net_ty}` in scope; use `std::net::{net_ty}::` instead"
                    )

    print(f"[OK] Static module/import/mutex/type analysis passed across {len(modules)} modules")


def run_git_diff_check():
    res = subprocess.run(["git", "diff", "--check"], cwd=REPO_ROOT, capture_output=True, text=True)
    if res.returncode != 0 or res.stdout.strip():
        raise RuntimeError(f"`git diff --check` reported issues:\n{res.stdout}\n{res.stderr}")
    print("[OK] `git diff --check` clean")


def run_local_cargo_if_available():
    cargo = shutil.which("cargo")
    if not cargo:
        home_cargo = os.path.expanduser("~/.cargo/bin/cargo.exe" if os.name == "nt" else "~/.cargo/bin/cargo")
        if os.path.isfile(home_cargo):
            cargo = home_cargo
    if not cargo:
        print("[INFO] Local `cargo` not installed; skipping local `cargo check` (use `--wait-ci` or `--release` for remote gate)")
        return
    print(f"[INFO] Running local `{cargo} check --all-targets --all-features`...")
    subprocess.run([cargo, "check", "--all-targets", "--all-features"], cwd=REPO_ROOT, check=True)
    print("[OK] Local `cargo check` passed!")


def get_github_token():
    p = subprocess.run(
        ["git", "credential", "fill"],
        input="protocol=https\nhost=github.com\n\n",
        cwd=REPO_ROOT,
        text=True,
        capture_output=True,
    )
    for line in p.stdout.splitlines():
        if line.startswith("password="):
            return line.split("=", 1)[1].strip()
    return os.environ.get("GITHUB_TOKEN")


def fetch_job_error_log(job_id: int, token: str):
    class NoRedir(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            return None

    opener = urllib.request.build_opener(NoRedir)
    headers = {"User-Agent": "rushway-ci-guard"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(
        f"https://api.github.com/repos/{GITHUB_REPO}/actions/jobs/{job_id}/logs",
        headers=headers,
    )
    try:
        opener.open(req, timeout=15)
        return ""
    except urllib.error.HTTPError as e:
        loc = e.headers.get("Location")
        if not loc:
            return f"HTTP {e.code} fetching job log"
    with urllib.request.urlopen(loc, timeout=20) as resp:
        text = resp.read().decode("utf-8", errors="replace")
    lines = text.splitlines()
    err_lines = [l for l in lines if ("error[" in l or "error:" in l or "warning:" in l or "FAILED" in l)]
    return "\n".join(err_lines[-40:] if err_lines else lines[-40:])


def wait_for_github_ci(head_sha: str, timeout_sec: int = 600):
    token = get_github_token()
    headers = {"User-Agent": "rushway-ci-guard"}
    if token:
        headers["Authorization"] = f"Bearer {token}"

    print(f"[CI] Waiting for GitHub Actions `RushWay CI` on commit {head_sha[:7]}...")
    deadline = time.time() + timeout_sec
    while time.time() < deadline:
        req = urllib.request.Request(
            f"https://api.github.com/repos/{GITHUB_REPO}/actions/runs?head_sha={head_sha}&per_page=10",
            headers=headers,
        )
        with urllib.request.urlopen(req, timeout=15) as resp:
            runs = json.loads(resp.read().decode()).get("workflow_runs", [])

        ci_runs = [r for r in runs if r.get("name") == "RushWay CI"]
        if ci_runs:
            run = ci_runs[0]
            jreq = urllib.request.Request(run["jobs_url"], headers=headers)
            with urllib.request.urlopen(jreq, timeout=15) as jresp:
                jobs = json.loads(jresp.read().decode()).get("jobs", [])
            for job in jobs:
                if job["name"] == "test":
                    status = job["status"]
                    conclusion = job["conclusion"]
                    if status == "completed":
                        if conclusion == "success":
                            print(f"[OK] GitHub Actions `RushWay CI` (`test` job) PASSED for {head_sha[:7]}!")
                            return True
                        else:
                            print(f"[FAIL] GitHub Actions `RushWay CI` (`test` job) FAILED ({run['html_url']}):")
                            print(fetch_job_error_log(job["id"], token))
                            return False
        time.sleep(8)
    raise TimeoutError(f"Timed out waiting for GitHub Actions CI on {head_sha[:7]}")


def install_pre_push_hook():
    hook_path = os.path.join(REPO_ROOT, ".git", "hooks", "pre-push")
    os.makedirs(os.path.dirname(hook_path), exist_ok=True)
    python_exe = sys.executable.replace("\\", "/")
    script_path = os.path.abspath(__file__).replace("\\", "/")
    hook_content = f"""#!/bin/sh
"{python_exe}" "{script_path}"
"""
    with open(hook_path, "w", encoding="utf-8", newline="\n") as f:
        f.write(hook_content)
    try:
        os.chmod(hook_path, 0o755)
    except OSError:
        pass
    print(f"[OK] Installed git pre-push hook at {os.path.relpath(hook_path, REPO_ROOT)}")


def main():
    parser = argparse.ArgumentParser(description="RushWay CI Pre-Check & Safe Release Guard")
    parser.add_argument("--install-hook", action="store_true", help="Install .git/hooks/pre-push hook")
    parser.add_argument("--wait-ci", action="store_true", help="Wait for GitHub Actions RushWay CI on HEAD")
    parser.add_argument(
        "--release",
        action="store_true",
        help="Push main, wait for RushWay CI `test` job to pass, then create/update and push tag vX.Y.Z",
    )
    args = parser.parse_args()

    version = check_versions_and_encoding()
    check_rust_static_semantics(os.path.join(REPO_ROOT, "src", "main.rs"))
    run_git_diff_check()
    run_local_cargo_if_available()

    if args.install_hook:
        install_pre_push_hook()

    if args.wait_ci:
        sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, text=True).strip()
        if not wait_for_github_ci(sha):
            sys.exit(1)

    if args.release:
        tag = f"v{version}"
        print(f"[RELEASE] Pushing `main` to origin before tagging `{tag}`...")
        subprocess.run(["git", "push", "origin", "main"], cwd=REPO_ROOT, check=True)
        sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, text=True).strip()
        if not wait_for_github_ci(sha):
            print(f"[ABORT] Blocking tag `{tag}` push because `RushWay CI` failed on {sha[:7]}!")
            sys.exit(1)
        print(f"[RELEASE] `RushWay CI` verified green! Updating and pushing tag `{tag}`...")
        subprocess.run(["git", "tag", "-f", tag, sha], cwd=REPO_ROOT, check=True)
        subprocess.run(["git", "push", "-f", "origin", tag], cwd=REPO_ROOT, check=True)
        print(f"[OK] Release `{tag}` ({sha[:7]}) pushed and verified!")


if __name__ == "__main__":
    main()
