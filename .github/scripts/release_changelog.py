#!/usr/bin/env python3
"""Generate a changelog section and bump the version using an LLM on OpenRouter.

Run from the repo root in CI. Reads commits since the last `v*` tag, asks an
OpenRouter model (Haiku by default) to categorise them in Keep a Changelog
style and recommend a semver bump, then rewrites `CHANGELOG.md`, `Cargo.toml`
and `Cargo.lock`.

Outputs (written to $GITHUB_OUTPUT):
  released = "true" | "false"   whether there was anything to release
  version  = "X.Y.Z"            the new version (only when released)

Side effects when released:
  - CHANGELOG.md updated (new section inserted under [Unreleased])
  - Cargo.toml / Cargo.lock version bumped
  - release_notes.md written (body for the GitHub release)
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import urllib.request
import urllib.error
from datetime import date

REPO = "maziluiosif/oxi"
MODEL = os.environ.get("OPENROUTER_MODEL", "anthropic/claude-haiku-4.5")
API_URL = "https://openrouter.ai/api/v1/chat/completions"
CATEGORIES = ["Added", "Changed", "Deprecated", "Removed", "Fixed", "Security"]


def run(*args: str) -> str:
    return subprocess.run(args, capture_output=True, text=True, check=True).stdout.strip()


def set_output(name: str, value: str) -> None:
    path = os.environ.get("GITHUB_OUTPUT")
    if path:
        with open(path, "a", encoding="utf-8") as fh:
            fh.write(f"{name}={value}\n")
    print(f"::notice::{name}={value}")


def current_version() -> str:
    text = open("Cargo.toml", encoding="utf-8").read()
    m = re.search(r'(?m)^\s*version\s*=\s*"([^"]+)"', text)
    if not m:
        sys.exit("could not find version in Cargo.toml")
    return m.group(1)


def last_tag() -> str | None:
    try:
        return run("git", "describe", "--tags", "--abbrev=0", "--match", "v*")
    except subprocess.CalledProcessError:
        return None


def collect_commits(tag: str | None) -> tuple[str, list[str]]:
    rng = f"{tag}..HEAD" if tag else "HEAD"
    log = run("git", "log", rng, "--no-merges", "--pretty=format:- %s%n%b")
    files = run("git", "diff", "--name-only", f"{tag}..HEAD") if tag else run(
        "git", "ls-files"
    )
    file_list = [f for f in files.splitlines() if f][:60]
    return log.strip(), file_list


def bump_version(version: str, bump: str) -> str:
    major, minor, patch = (int(x) for x in version.split("."))
    if bump == "major":
        return f"{major + 1}.0.0"
    if bump == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def call_llm(commits: str, files: list[str]) -> dict:
    api_key = os.environ.get("OPENROUTER_API_KEY")
    if not api_key:
        sys.exit("OPENROUTER_API_KEY is not set")

    system = (
        "You are a release-notes generator for the open-source Rust desktop app "
        "'oxi'. You receive the raw git commits merged since the last release. "
        "Group the user-facing changes into Keep a Changelog categories and "
        "recommend a Semantic Versioning bump.\n\n"
        "Rules:\n"
        "- bump = 'major' if there is any breaking/removed public behaviour, "
        "'minor' if there are new features, otherwise 'patch'.\n"
        "- Only include changes that matter to users or contributors. Merge "
        "duplicate or noisy commits; drop pure CI/formatting churn unless it is "
        "notable.\n"
        "- Write concise, present-tense bullet points. Do NOT invent changes.\n"
        "- Use only these categories: Added, Changed, Deprecated, Removed, "
        "Fixed, Security. Omit empty ones.\n"
        "- Mark breaking changes by ending the bullet with ' **(breaking)**'.\n\n"
        "Respond with ONLY a JSON object of the form:\n"
        '{"bump":"patch","sections":{"Added":["..."],"Fixed":["..."]}}'
    )
    user = f"Changed files:\n{chr(10).join(files)}\n\nCommits:\n{commits}"

    payload = {
        "model": MODEL,
        "temperature": 0.2,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    }
    req = urllib.request.Request(
        API_URL,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
            "HTTP-Referer": f"https://github.com/{REPO}",
            "X-Title": "oxi release changelog",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            data = json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        sys.exit(f"OpenRouter HTTP {exc.code}: {exc.read().decode('utf-8', 'replace')}")

    content = data["choices"][0]["message"]["content"]
    return parse_llm_json(content)


def parse_llm_json(content: str) -> dict:
    content = content.strip()
    # Strip ```json fences if present, then grab the outermost JSON object.
    content = re.sub(r"^```(?:json)?|```$", "", content, flags=re.MULTILINE).strip()
    start, end = content.find("{"), content.rfind("}")
    if start == -1 or end == -1:
        sys.exit(f"LLM did not return JSON:\n{content}")
    obj = json.loads(content[start : end + 1])
    bump = obj.get("bump", "patch")
    if bump not in ("major", "minor", "patch"):
        bump = "patch"
    sections = {
        cat: [str(x).strip() for x in obj.get("sections", {}).get(cat, []) if str(x).strip()]
        for cat in CATEGORIES
    }
    sections = {k: v for k, v in sections.items() if v}
    return {"bump": bump, "sections": sections}


def render_section(version: str, sections: dict) -> str:
    lines = [f"## [{version}] - {date.today().isoformat()}", ""]
    for cat in CATEGORIES:
        if cat in sections:
            lines.append(f"### {cat}")
            lines.extend(f"- {item}" for item in sections[cat])
            lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def update_changelog(version: str, prev: str | None, section: str) -> str:
    text = open("CHANGELOG.md", encoding="utf-8").read()

    # Insert the new section directly after the "## [Unreleased]" heading.
    marker = "## [Unreleased]"
    idx = text.find(marker)
    if idx == -1:
        sys.exit("CHANGELOG.md is missing the '## [Unreleased]' heading")
    insert_at = text.find("\n", idx) + 1
    text = text[:insert_at] + "\n" + section + "\n" + text[insert_at:]

    # Maintain the reference links at the bottom of the file.
    base = f"https://github.com/{REPO}"
    text = re.sub(
        r"(?m)^\[Unreleased\]:.*$",
        f"[Unreleased]: {base}/compare/v{version}...HEAD",
        text,
    )
    if prev:
        new_link = f"[{version}]: {base}/compare/v{prev}...v{version}"
    else:
        new_link = f"[{version}]: {base}/releases/tag/v{version}"
    text = re.sub(
        r"(?m)^(\[Unreleased\]:.*)$",
        lambda m: m.group(1) + "\n" + new_link,
        text,
        count=1,
    )

    open("CHANGELOG.md", "w", encoding="utf-8").write(text)
    return section


def update_cargo(prev: str, version: str) -> None:
    toml = open("Cargo.toml", encoding="utf-8").read()
    toml = re.sub(
        r'(?m)^(\s*version\s*=\s*)"%s"' % re.escape(prev),
        r'\g<1>"%s"' % version,
        toml,
        count=1,
    )
    open("Cargo.toml", "w", encoding="utf-8").write(toml)

    lock = open("Cargo.lock", encoding="utf-8").read()
    lock = re.sub(
        r'(name = "oxi"\nversion = )"%s"' % re.escape(prev),
        r'\g<1>"%s"' % version,
        lock,
        count=1,
    )
    open("Cargo.lock", "w", encoding="utf-8").write(lock)


def main() -> None:
    prev = current_version()
    tag = last_tag()
    commits, files = collect_commits(tag)

    if not commits:
        print("No commits since last release; nothing to do.")
        set_output("released", "false")
        return

    result = call_llm(commits, files)
    if not result["sections"]:
        print("LLM found no user-facing changes; nothing to release.")
        set_output("released", "false")
        return

    version = bump_version(prev, result["bump"])
    section = render_section(version, result["sections"])
    body = update_changelog(version, tag.lstrip("v") if tag else None, section)
    update_cargo(prev, version)
    open("release_notes.md", "w", encoding="utf-8").write(body)

    print(f"Released v{version} (bump={result['bump']}, from v{prev})")
    set_output("released", "true")
    set_output("version", version)


if __name__ == "__main__":
    main()
