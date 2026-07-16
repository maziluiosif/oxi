#!/usr/bin/env python3
"""Unit tests for deterministic parts of release_changelog.py."""

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).with_name("release_changelog.py")
SPEC = importlib.util.spec_from_file_location("release_changelog", SCRIPT)
assert SPEC and SPEC.loader
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


class ReleaseChangelogTests(unittest.TestCase):
    def test_fallback_groups_conventional_commits_and_ignores_maintenance(self) -> None:
        result = release.fallback_notes(
            "- feat(agent): add reversible tools\n\n"
            "- fix: prevent startup crash\n\n"
            "- chore: format files\n"
        )
        self.assertEqual(
            result,
            {
                "bump": "minor",
                "sections": {
                    "Added": ["Add reversible tools"],
                    "Fixed": ["Prevent startup crash"],
                },
            },
        )

    def test_fallback_detects_breaking_change(self) -> None:
        result = release.fallback_notes("- feat(api)!: replace provider format")
        self.assertEqual(result["bump"], "major")

    def test_fallback_keeps_non_conventional_commits(self) -> None:
        result = release.fallback_notes("- improve settings layout")
        self.assertEqual(
            result,
            {"bump": "patch", "sections": {"Changed": ["Improve settings layout"]}},
        )

    def test_parse_llm_json_accepts_fenced_response(self) -> None:
        result = release.parse_llm_json(
            '```json\n{"bump":"minor","sections":{"Added":["New tool"]}}\n```'
        )
        self.assertEqual(
            result, {"bump": "minor", "sections": {"Added": ["New tool"]}}
        )

    def test_main_skips_history_only_changes(self) -> None:
        with mock.patch.object(release, "current_version", return_value="1.2.3"), mock.patch.object(
            release, "last_tag", return_value="v1.2.3"
        ), mock.patch.object(
            release,
            "collect_commits",
            return_value=("- Merge release metadata back to dev", []),
        ), mock.patch.object(release, "call_llm") as call_llm, mock.patch.object(
            release, "set_output"
        ) as set_output:
            release.main()

        call_llm.assert_not_called()
        set_output.assert_called_once_with("released", "false")

    def test_update_cargo_changes_only_oxi_versions(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Cargo.toml").write_text(
                '[package]\nname = "oxi"\nversion = "1.2.3"\n', encoding="utf-8"
            )
            (root / "Cargo.lock").write_text(
                '[[package]]\nname = "dep"\nversion = "1.2.3"\n\n'
                '[[package]]\nname = "oxi"\nversion = "1.2.3"\n',
                encoding="utf-8",
            )
            old_cwd = Path.cwd()
            try:
                os.chdir(root)
                release.update_cargo("1.2.3", "1.3.0")
            finally:
                os.chdir(old_cwd)

            self.assertIn('version = "1.3.0"', (root / "Cargo.toml").read_text())
            lock = (root / "Cargo.lock").read_text()
            self.assertIn('name = "dep"\nversion = "1.2.3"', lock)
            self.assertIn('name = "oxi"\nversion = "1.3.0"', lock)


if __name__ == "__main__":
    unittest.main()
