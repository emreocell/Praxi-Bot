"""Exercise the guide index checker in a temporary Git checkout."""

from pathlib import Path
import shutil
import subprocess
import sys
from tempfile import TemporaryDirectory
from textwrap import dedent
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]


class GuideIndexCheckerTests(unittest.TestCase):
    def run_checker(self, files):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True, capture_output=True)
            for script in ("check_guide_index.py", "check_markdown_links.py"):
                shutil.copy(SCRIPTS / script, root / script)
            for name, content in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(dedent(content).lstrip("\n"), encoding="utf-8")
            subprocess.run(["git", "add", "--", "."], cwd=root, check=True, capture_output=True)
            return subprocess.run(
                [sys.executable, str(root / "check_guide_index.py")], cwd=root, capture_output=True,
                text=True, encoding="utf-8",
            )

    def test_passes_when_every_page_is_linked(self):
        result = self.run_checker({
            "docs/guide/index.md": """
                | You want to... | Read |
                | --- | --- |
                | Install | [Install](install.md) |
                | Chat | [Chatting](./chatting.md#approvals) |
            """,
            "docs/guide/install.md": "# Install\n",
            "docs/guide/chatting.md": "# Chatting\n## Approvals\n",
            "docs/guide/images/notes.md": "Subfolders are not checked.\n",
        })
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("All guide pages are linked", result.stdout)

    def test_names_unlinked_pages(self):
        result = self.run_checker({
            "docs/guide/index.md": """
                [Install](install.md)
                [Elsewhere](../other/calendar.md)
                `[In code](inline.md)`

                ```
                [In a fence](fenced.md)
                ```
            """,
            "docs/guide/install.md": "# Install\n",
            "docs/guide/calendar.md": "# Calendar\n",
            "docs/guide/inline.md": "# Inline\n",
            "docs/guide/fenced.md": "# Fenced\n",
            "docs/guide/release-linux.md": "# Allowed to stay unlinked\n",
        })
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(
            [line for line in result.stdout.splitlines() if ": not linked" in line],
            [
                "docs/guide/calendar.md: not linked from docs/guide/index.md",
                "docs/guide/fenced.md: not linked from docs/guide/index.md",
                "docs/guide/inline.md: not linked from docs/guide/index.md",
            ],
        )


if __name__ == "__main__":
    unittest.main()
