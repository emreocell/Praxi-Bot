"""Hermetic tests for the MCP catalog verifier.

The offline checks run on fixtures and on the real shipped catalog. The live checks talk only to
a throw-away server on 127.0.0.1 (registry, homepage and redirect routes); no real vendor,
registry or credential is involved.
"""

from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
from pathlib import Path
import sys
from tempfile import TemporaryDirectory
import threading
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "verify_mcp_catalog.py"
CATALOG = ROOT / "crates" / "mcp-bridge" / "catalog.json"
WORKFLOW = ROOT / ".github" / "workflows" / "mcp-catalog-check.yml"
CI = ROOT / ".github" / "workflows" / "ci.yml"


def load_module():
    spec = importlib.util.spec_from_file_location("verify_mcp_catalog", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


verify = load_module()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def _send(self, code, body=b"", headers=None):
        self.send_response(code)
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):  # noqa: N802
        path = self.path
        if path == "/home":
            self._send(200, b"<html></html>")
        elif path == "/gone":
            self._send(404)
        elif path == "/blocked":
            self._send(403)
        elif path == "/hop":
            self._send(302, headers={"Location": "/home"})
        elif path == "/loop":
            self._send(302, headers={"Location": "/loop"})
        elif path == "/npm/@scope%2Fpkg/1.2.3":
            self._send(200, json.dumps({"license": "MIT"}).encode())
        elif path == "/npm/plain/2.0.0":
            self._send(200, json.dumps({"license": "SEE LICENSE IN LICENSE"}).encode())
        elif path == "/pypi/tool/0.3.0/json":
            self._send(200, json.dumps({"info": {"license_expression": "Apache-2.0"}}).encode())
        else:
            self._send(404)


@contextmanager
def server():
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{httpd.server_address[1]}"
    finally:
        httpd.shutdown()
        httpd.server_close()


def entry(**changes):
    base = {
        "id": "demo",
        "name": "Demo",
        "description": "A demo.",
        "category": "accounts",
        "homepage": "https://example.com/demo",
        "icon": None,
        "license": "hosted",
        "auth": "oauth-dcr",
        "install": {"transport": "http", "url": "https://mcp.example.com/mcp", "required_secrets": []},
    }
    base.update(changes)
    return base


def local(args, license_="MIT", auth="none", command="npx", secrets=None):
    install = {"transport": "stdio", "command": command, "args": args, "required_secrets": secrets or []}
    return entry(id="local", license=license_, auth=auth, install=install)


def fake_root(directory: str, icons="siGithub, siLinear", notices="Slack: https://slack.com/media-kit") -> Path:
    root = Path(directory)
    (root / "apps/desktop/ui/src/lib").mkdir(parents=True)
    (root / "apps/desktop/ui/src/lib/brandIcons.ts").write_text(
        f"import {{ {icons} }} from 'simple-icons';\n", encoding="utf-8"
    )
    (root / "THIRD_PARTY_NOTICES.md").write_text(notices, encoding="utf-8")
    return root


def problems(*entries, **root_args):
    with TemporaryDirectory() as tmp:
        return verify.offline_errors(list(entries), fake_root(tmp, **root_args))


class PackagePinTests(unittest.TestCase):
    def test_pinned_forms(self):
        cases = [
            ("npx", ["-y", "@scope/pkg@1.2.3"], ("npm", "@scope/pkg", "1.2.3")),
            ("npx", ["-y", "plain@2.0.0", "--flag"], ("npm", "plain", "2.0.0")),
            ("npx", ["-y", "-p", "@todoist/mcp@17.2.0", "todoist-mcp"], ("npm", "@todoist/mcp", "17.2.0")),
            ("npx.cmd", ["--yes", "pkg@1.0.0-rc.1"], ("npm", "pkg", "1.0.0-rc.1")),
            ("uvx", ["tool==0.3.0", "--db-path", "./x.db"], ("pypi", "tool", "0.3.0")),
            ("uvx", ["tool[extra]==0.3.0"], ("pypi", "tool", "0.3.0")),
            ("uv", ["tool", "run", "tool==0.3.0"], ("pypi", "tool", "0.3.0")),
            ("pnpm", ["dlx", "plain@2.0.0"], ("npm", "plain", "2.0.0")),
        ]
        for command, args, want in cases:
            self.assertEqual(verify.package_of({"command": command, "args": args}), want, (command, args))

    def test_unpinned_forms_say_why(self):
        for command, args in [
            ("npx", ["-y", "pkg"]),
            ("npx", ["-y", "pkg@latest"]),
            ("npx", ["-y", "pkg@^1.2.3"]),
            ("npx", ["-y", "pkg@1.2"]),
            ("uvx", ["tool"]),
            ("uvx", ["tool>=1.0.0"]),
            ("npx", ["-y"]),
        ]:
            result = verify.package_of({"command": command, "args": args})
            self.assertIsInstance(result, str, (command, args))

    def test_other_commands_are_not_package_runners(self):
        self.assertIsNone(verify.package_of({"command": "/usr/local/bin/my-server", "args": []}))


class OfflineTests(unittest.TestCase):
    def test_a_good_remote_and_a_good_local_entry_pass(self):
        self.assertEqual(problems(entry(), local(["-y", "pkg@1.2.3"])), [])

    def test_license_and_auth_must_be_noted(self):
        e = entry()
        del e["license"]
        self.assertIn("license is not noted", " ".join(problems(e)))
        e = entry()
        del e["auth"]
        self.assertIn("not declared", " ".join(problems(e)))
        self.assertIn("not declared", " ".join(problems(entry(auth="magic"))))
        self.assertIn("neither `hosted` nor an SPDX", " ".join(problems(local(["-y", "p@1.0.0"], license_="see file"))))

    def test_auth_must_agree_with_the_recipe(self):
        secrets = [{"name": "mcp/key", "label": "Key"}]
        remote_secret = entry(auth="oauth-dcr")
        remote_secret["install"]["required_secrets"] = secrets
        self.assertIn("does not match", " ".join(problems(remote_secret)))
        keyless = entry(auth="api-key")
        self.assertIn("does not match", " ".join(problems(keyless)))
        free_but_secret = local(["-y", "p@1.0.0"], auth="none", secrets=secrets)
        self.assertIn("does not match", " ".join(problems(free_but_secret)))
        self.assertEqual(problems(local(["-y", "p@1.0.0"], auth="api-key", secrets=secrets)), [])
        own = entry(auth="oauth-own-client")
        self.assertIn("does not match", " ".join(problems(own)))
        own["install"]["oauth_client"] = {"issuer": "https://accounts.example.com", "scopes": ["r"], "setup": "x"}
        self.assertEqual(problems(own), [])

    def test_hosted_only_for_remote_entries(self):
        self.assertIn("must be `hosted`", " ".join(problems(entry(license="MIT"))))
        self.assertIn("not `hosted`", " ".join(problems(local(["-y", "p@1.0.0"], license_="hosted"))))

    def test_logo_licence_must_be_known(self):
        self.assertEqual(problems(entry(icon="github")), [])  # Simple Icons: CC0
        self.assertEqual(problems(entry(icon=None)), [])  # neutral initials
        self.assertEqual(problems(entry(icon="slack")), [])  # recorded in THIRD_PARTY_NOTICES.md
        self.assertIn("no licence noted", " ".join(problems(entry(icon="slack"), notices="")))
        self.assertIn("no licence noted", " ".join(problems(entry(icon="acme-logo"))))

    def test_unpinned_package_id_and_homepage_are_errors(self):
        self.assertIn("not pinned", " ".join(problems(local(["-y", "pkg"]))))
        self.assertIn("lower-case words", " ".join(problems(entry(id="Bad_Id"))))
        self.assertIn("homepage must be an https URL", " ".join(problems(entry(homepage="http://example.com"))))
        self.assertIn("missing description", " ".join(problems(entry(description=" "))))

    def test_shared_structural_rules_still_apply(self):
        joined = " ".join(problems(entry(), entry()))
        self.assertIn("duplicate id", joined)

    def test_the_shipped_catalog_is_clean(self):
        catalog = json.loads(CATALOG.read_text(encoding="utf-8"))
        self.assertEqual(verify.offline_errors(catalog), [])
        self.assertEqual(verify.docs_errors(catalog), [])
        self.assertGreaterEqual(len(catalog), 40)


class DocsTests(unittest.TestCase):
    def pages(self, root: Path, catalog: list, drift=False):
        for lang, relative in verify.DOCS.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            table = verify.render_docs_table(catalog, lang) + ("\nextra" if drift else "")
            path.write_text(f"# t\n{verify.TABLE_BEGIN}\n{table}\n{verify.TABLE_END}\n", encoding="utf-8")

    def test_table_lists_every_entry_with_its_sign_in_in_both_languages(self):
        catalog = [entry(), local(["-y", "p@1.0.0"], auth="api-key", secrets=[{"name": "mcp/k", "label": "K"}])]
        en = verify.render_docs_table(catalog, "en")
        tr = verify.render_docs_table(catalog, "tr")
        self.assertIn("Sign in with your account in the browser", en)
        self.assertIn("Paste an API key", en)
        self.assertIn("Hosted by the vendor", en)
        self.assertIn("Tarayıcıda hesabınla oturum aç", tr)
        self.assertIn("Sağlayıcı barındırır", tr)
        self.assertEqual(len(en.splitlines()), 4)

    def test_drift_missing_page_and_markers_are_reported(self):
        catalog = [entry()]
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.assertIn("guide page is missing", " ".join(verify.docs_errors(catalog, root)))
            self.pages(root, catalog)
            self.assertEqual(verify.docs_errors(catalog, root), [])
            self.pages(root, catalog, drift=True)
            self.assertIn("out of date", " ".join(verify.docs_errors(catalog, root)))
            (root / verify.DOCS["en"]).write_text("no markers", encoding="utf-8")
            self.assertIn("markers", " ".join(verify.docs_errors(catalog, root)))


class LiveTests(unittest.TestCase):
    def test_homepage_redirects_blocking_and_dead_pages(self):
        with server() as base:
            check = lambda path: verify.check_homepage(base + path, 1, allow_loopback=True)  # noqa: E731
            self.assertEqual(check("/home")[0], "ok")
            self.assertEqual(check("/hop")[0], "ok")  # one redirect is followed
            self.assertEqual(check("/blocked")[0], "ok")  # the site answers, it just refuses bots
            self.assertEqual(check("/gone")[0], "homepage-unreachable")
            status, note = check("/loop")
            self.assertEqual(status, "homepage-unreachable")
            self.assertIn("too many redirects", note)

    def test_a_redirect_to_a_refused_target_is_not_followed(self):
        # Without allow_loopback a loopback hop is refused before any request is made.
        with server() as base:
            status, note = verify.check_homepage(base + "/hop", 1)
            self.assertEqual(status, "homepage-unreachable")
            self.assertIn("refused url", note)

    def test_package_exists_missing_and_license_mismatch(self):
        with server() as base, mock.patch.object(verify, "NPM_REGISTRY", base + "/npm"), mock.patch.object(
            verify, "PYPI_REGISTRY", base + "/pypi"
        ):
            run = lambda args, lic, cmd="npx": verify.check_package(  # noqa: E731
                {"command": cmd, "args": args}, lic, 1, allow_loopback=True
            )
            self.assertEqual(run(["-y", "@scope/pkg@1.2.3"], "MIT")[0], "ok")
            self.assertEqual(run(["-y", "plain@2.0.0"], "MIT")[0], "ok")  # "SEE LICENSE IN" says nothing
            self.assertEqual(run(["tool==0.3.0"], "Apache-2.0", "uvx")[0], "ok")
            self.assertEqual(run(["-y", "ghost@9.9.9"], "MIT")[0], "package-missing")
            status, note = run(["-y", "@scope/pkg@1.2.3"], "Apache-2.0")
            self.assertEqual(status, "license-mismatch")
            self.assertIn("MIT", note)
            self.assertEqual(run(["-y", "pkg"], "MIT")[0], "package-missing")
            self.assertEqual(run([], "MIT", "my-binary")[0], "ok")

    def test_registry_urls_encode_scoped_names(self):
        self.assertEqual(
            verify.registry_url("npm", "@scope/pkg", "1.2.3"), "https://registry.npmjs.org/@scope%2Fpkg/1.2.3"
        )
        self.assertEqual(verify.registry_url("pypi", "tool", "0.3.0"), "https://pypi.org/pypi/tool/0.3.0/json")

    def test_declared_auth_is_compared_with_the_endpoint(self):
        self.assertIsNone(verify.auth_mismatch("oauth-dcr", {"status": "oauth-dcr"}))
        self.assertIsNone(verify.auth_mismatch("oauth-own-client", {"status": "open"}))
        self.assertIsNone(verify.auth_mismatch("api-key", {"status": "api-key"}))
        self.assertIsNone(verify.auth_mismatch("none", {"status": "open"}))
        self.assertIn("oauth-no-dcr", verify.auth_mismatch("oauth-dcr", {"status": "oauth-no-dcr"}))
        self.assertIn("declared none", verify.auth_mismatch("none", {"status": "oauth-dcr"}))
        self.assertIn("api-key", verify.auth_mismatch("api-key", {"status": "oauth-dcr"}))

    def test_remote_entry_row_combines_homepage_endpoint_and_declared_auth(self):
        with server() as base:
            e = entry(homepage=base + "/home")
            for observed, want in [
                ({"status": "oauth-dcr", "auth": "oauth-dcr", "dcr": True, "note": ""}, "ok"),
                ({"status": "oauth-no-dcr", "auth": "oauth-no-dcr", "dcr": False, "note": "no registration_endpoint"}, "oauth-no-dcr"),
                ({"status": "unreachable", "auth": "none", "dcr": None, "note": "unreachable: boom"}, "unreachable"),
            ]:
                with mock.patch.object(verify.endpoint_check, "check_endpoint", return_value=observed):
                    row = verify.verify_entry_live(e, 1, True)
                self.assertEqual(row["status"], want)
            e = entry(homepage=base + "/gone", auth="none")
            ok = {"status": "oauth-dcr", "auth": "oauth-dcr", "dcr": True, "note": ""}
            with mock.patch.object(verify.endpoint_check, "check_endpoint", return_value=ok):
                row = verify.verify_entry_live(e, 1, True)
            self.assertEqual(row["status"], "homepage-unreachable")
            self.assertEqual(len(row["notes"]), 2)  # the dead homepage and the declared-none contradiction

    def test_a_checker_crash_becomes_a_row(self):
        with server() as base:
            with mock.patch.object(verify.endpoint_check, "check_endpoint", side_effect=RuntimeError("x")):
                row = verify.verify_entry_live(entry(homepage=base + "/home"), 1, True)
            self.assertEqual(row["status"], "bad-response")
            self.assertIn("RuntimeError", row["notes"][0])


class ReportTests(unittest.TestCase):
    def write(self, tmp: str, catalog: list) -> tuple[Path, Path]:
        root = fake_root(Path(tmp, "root").as_posix())
        for lang, relative in verify.DOCS.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                # The page always matches a good catalog; a bad entry cannot be rendered.
                f"{verify.TABLE_BEGIN}\n{verify.render_docs_table([e for e in catalog if 'license' in e], lang)}\n{verify.TABLE_END}\n",
                encoding="utf-8",
            )
        path = Path(tmp) / "catalog.json"
        path.write_text(json.dumps(catalog), encoding="utf-8")
        return path, root

    def test_offline_report_schema_and_exit_codes(self):
        with TemporaryDirectory() as tmp:
            catalog = [entry(), local(["-y", "pkg@1.2.3"])]
            path, root = self.write(tmp, catalog)
            out = Path(tmp) / "report.json"
            report = verify.run_verify(path, root)
            self.assertEqual(report["mode"], "offline")
            self.assertEqual(report["structural_errors"], [])
            self.assertEqual(report["attention"], [])
            self.assertEqual({r["id"] for r in report["entries"]}, {"demo", "local"})
            self.assertEqual(verify.main(["--catalog", str(path), "--root", str(root), "--json", str(out)]), 0)
            written = json.loads(out.read_text(encoding="utf-8"))
            for key in ("checked_at", "entries", "attention", "structural_errors", "mode"):
                self.assertIn(key, written)
            self.assertEqual(verify.main(["--catalog", str(path), "--root", str(root), "--only", "demo"]), 0)

    def test_a_bad_catalog_exits_2_and_a_missing_one_exits_1(self):
        with TemporaryDirectory() as tmp:
            bad = entry()
            del bad["license"]
            path, root = self.write(tmp, [bad])
            self.assertEqual(verify.main(["--catalog", str(path), "--root", str(root)]), 2)
            self.assertEqual(verify.main(["--catalog", str(Path(tmp) / "none.json"), "--root", str(root)]), 1)

    def test_live_attention_is_reported_but_only_strict_fails(self):
        with TemporaryDirectory() as tmp, server() as base:
            catalog = [entry(homepage=base + "/gone", install={"transport": "http", "url": base + "/x", "required_secrets": []})]
            path, root = self.write(tmp, catalog)
            dcr = {"status": "oauth-dcr", "auth": "oauth-dcr", "dcr": True, "note": ""}
            # The loopback homepage is not an https URL, so the offline rules are stubbed out here.
            with mock.patch.object(verify.endpoint_check, "check_endpoint", return_value=dcr), mock.patch.object(
                verify, "offline_errors", return_value=[]
            ), mock.patch.object(verify, "docs_errors", return_value=[]):
                args = ["--catalog", str(path), "--root", str(root), "--live", "--timeout", "1", "--allow-loopback"]
                report = verify.run_verify(path, root, live=True, timeout=1, allow_loopback=True)
                self.assertEqual([a["id"] for a in report["attention"]], ["demo"])
                self.assertEqual(verify.main(args), 0)
                self.assertEqual(verify.main(args + ["--strict"]), 1)

    def test_render_docs_prints_the_table(self):
        with TemporaryDirectory() as tmp:
            path, root = self.write(tmp, [entry()])
            with mock.patch("builtins.print") as printed:
                self.assertEqual(verify.main(["--catalog", str(path), "--root", str(root), "--render-docs", "en"]), 0)
            self.assertIn("| [Demo](https://example.com/demo) |", printed.call_args[0][0])


class WorkflowTests(unittest.TestCase):
    def test_scheduled_workflow_runs_the_verifier_live_and_never_on_pull_requests(self):
        text = WORKFLOW.read_text(encoding="utf-8")
        self.assertIn("scripts/verify_mcp_catalog.py --live", text)
        self.assertNotIn("pull_request:", text)

    def test_ci_runs_the_offline_verification(self):
        ci = CI.read_text(encoding="utf-8")
        self.assertIn("python3 scripts/verify_mcp_catalog.py\n", ci)
        self.assertNotIn("verify_mcp_catalog.py --live", ci)
        self.assertIn("test_verify_mcp_catalog.py", ci)


if __name__ == "__main__":
    unittest.main()
