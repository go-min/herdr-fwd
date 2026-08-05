#!/usr/bin/env python3
"""Validate repository invariants that TOML parsing alone cannot catch."""

from pathlib import Path
import re
import sys
import tomllib


ROOT = Path(__file__).resolve().parent.parent
PLUGIN_ID = "herdr.fwd"
REQUIRED_EVENTS = {"pane.created", "pane.closed", "pane.exited", "pane.focused"}


def fail(message: str) -> None:
    print(f"manifest check: {message}", file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    with (ROOT / "Cargo.toml").open("rb") as stream:
        cargo = tomllib.load(stream)
    with (ROOT / "herdr-plugin.toml").open("rb") as stream:
        plugin = tomllib.load(stream)

    package = cargo["package"]
    for field in ("name", "version", "license", "repository", "rust-version"):
        if not package.get(field):
            fail(f"Cargo package field {field!r} is required")
    if package.get("publish") is not False:
        fail("the paired wrapper/plugin package must not be published to crates.io")

    if plugin.get("id") != PLUGIN_ID:
        fail(f"plugin id must be {PLUGIN_ID!r}")
    if plugin.get("version") != package["version"]:
        fail("Cargo and plugin versions differ")
    if not plugin.get("min_herdr_version"):
        fail("min_herdr_version is required")
    if set(plugin.get("platforms", [])) != {"linux", "macos"}:
        fail("top-level platforms must be exactly linux and macos")

    build_commands = [entry.get("command", []) for entry in plugin.get("build", [])]
    if ["./scripts/install-plugin-binary.sh"] not in build_commands:
        fail("managed installs must use the prebuilt plugin installer")
    plugin_installer = ROOT / "scripts/install-plugin-binary.sh"
    if not plugin_installer.is_file() or plugin_installer.stat().st_mode & 0o111 == 0:
        fail("prebuilt plugin installer must exist and be executable")

    action_ids = [entry.get("id") for entry in plugin.get("actions", [])]
    if len(action_ids) != len(set(action_ids)) or None in action_ids:
        fail("action ids must be present and unique")

    pane_ids = [entry.get("id") for entry in plugin.get("panes", [])]
    if len(pane_ids) != len(set(pane_ids)) or None in pane_ids:
        fail("pane ids must be present and unique")
    welcome = next(
        (entry for entry in plugin.get("panes", []) if entry.get("id") == "welcome"),
        None,
    )
    if welcome is None or welcome.get("placement") != "popup":
        fail("welcome must be a session-modal plugin popup")
    if welcome.get("command") != ["./target/release/herdr-fwd-plugin", "welcome"]:
        fail("welcome popup must use the onboarding entrypoint")

    startup_commands = [entry.get("command") for entry in plugin.get("startup", [])]
    start_command = ["./target/release/herdr-fwd-plugin", "start"]
    if startup_commands != [start_command]:
        fail("plugin startup must run the combined watcher/onboarding entrypoint")

    events = plugin.get("events", [])
    event_names = [entry.get("on") for entry in events]
    if set(event_names) != REQUIRED_EVENTS or len(event_names) != len(REQUIRED_EVENTS):
        fail("plugin lifecycle events must cover create, close, exit, and focus exactly once")
    for entry in events:
        if entry.get("command") != start_command:
            fail("plugin lifecycle events must run the combined start entrypoint")

    for section in ("startup", "actions", "panes", "events"):
        for entry in plugin.get(section, []):
            command = entry.get("command", [])
            if not command or not command[0].startswith("./"):
                fail(f"{section} command must use a plugin-relative ./ executable")

    workflows = sorted((ROOT / ".github/workflows").glob("*.yml"))
    if not workflows:
        fail("at least one GitHub Actions workflow is required")
    action_pattern = re.compile(r"\buses:\s*[^@\s]+@([0-9a-f]{40})(?:\s|$)")
    for workflow in workflows:
        for number, line in enumerate(workflow.read_text().splitlines(), 1):
            if "uses:" in line and "uses: ./" not in line:
                if not action_pattern.search(line):
                    fail(
                        f"{workflow.relative_to(ROOT)}:{number} action must use a 40-character SHA"
                    )

    release = (ROOT / ".github/workflows/release.yml").read_text()
    ci = (ROOT / ".github/workflows/ci.yml").read_text()
    if "cargo +1.82 check" not in ci:
        fail("MSRV workflow must invoke the explicit Rust 1.82 toolchain")
    for platform in (
        "linux-x86_64",
        "linux-aarch64",
        "macos-x86_64",
        "macos-aarch64",
    ):
        if platform not in release:
            fail(f"release workflow is missing {platform}")
    for artifact in ("SHA256SUMS", "spdx-json", "attest-build-provenance"):
        if artifact not in release:
            fail(f"release workflow is missing {artifact}")
    if re.search(
        r"if:\s*\$\{\{\s*false\s*\}\}\n\s+uses: actions/attest-build-provenance@",
        release,
    ):
        fail("release workflow must enable build provenance attestations")

    for job in ("homebrew-preflight", "homebrew-test", "homebrew"):
        if f"  {job}:" not in release:
            fail(f"release workflow is missing {job}")
        if re.search(
            rf"^  {re.escape(job)}:\n    if:\\s*\\$\\{{\\{{\\s*false\\s*\\}}\\}}",
            release,
            re.MULTILINE,
        ):
            fail(f"release workflow must enable {job}")
    if "needs: [build, homebrew-test]" not in release:
        fail("GitHub Release publication must require Homebrew package tests")
    if "needs: [publish, homebrew-preflight, homebrew-test]" not in release:
        fail("Homebrew publication must require release, access, and package tests")
    for required in (
        "workflow_dispatch:",
        "RELEASE_TAG: ${{ inputs.tag || github.ref_name }}",
        "ref: ${{ env.RELEASE_TAG }}",
    ):
        if required not in release:
            fail(f"release recovery workflow is missing {required!r}")
    if "GITHUB_REF_NAME" in release:
        fail("release workflow must use the validated release tag in every job")
    identity = release.find("git config --global user.name github-actions[bot]")
    tap_creation = release.find("brew tap-new")
    if identity < 0 or identity > tap_creation:
        fail("Homebrew package tests must configure Git identity before creating a tap")
    manual_homebrew = ROOT / ".github/workflows/homebrew.yml"
    if not manual_homebrew.is_file():
        fail("manual Homebrew publication workflow is required")
    manual_homebrew_text = manual_homebrew.read_text()
    for required in (
        "workflow_dispatch:",
        "HOMEBREW_TAP_TOKEN",
        "gh release download",
        "gh pr create",
        "ref: ${{ inputs.tag }}",
    ):
        if required not in manual_homebrew_text:
            fail(f"manual Homebrew workflow is missing {required!r}")


if __name__ == "__main__":
    main()
