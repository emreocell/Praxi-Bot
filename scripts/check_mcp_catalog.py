#!/usr/bin/env python3
"""Credential-free verification of the HTTP entries in the MCP catalog.

This is a *reporting* tool, not a gate on unrelated work: a vendor outage, a
changed endpoint or missing OAuth metadata becomes an "attention" line in the
report, not a failed build. Structural errors in catalog.json do fail (exit 2).

No credentials are sent, no response bodies are cached, and at most a handful
of short metadata requests are made per entry. Run it by hand with:

    python3 scripts/check_mcp_catalog.py --timeout 5

See docs/contributing/MAINTAINING.md for how to read the report.
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import ipaddress
import json
from pathlib import Path
import re
import socket
import ssl
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

USER_AGENT = "praxi-catalog-check/1 (+https://github.com/emreocell/Praxi-Bot)"
INITIALIZE = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "praxi-catalog-check", "version": "1"},
    },
}
INIT_HEADERS = {
    "Content-Type": "application/json",
    "Accept": "application/json, text/event-stream",
    "User-Agent": USER_AGENT,
}
MAX_METADATA_FETCHES = 4
MAX_BODY_BYTES = 65536
# The per-socket timeout alone lets a slow-drip body run for minutes; this caps one request overall.
TOTAL_DEADLINE_FACTOR = 3
TOKEN_PATTERNS = (
    re.compile(r"\bsk-[A-Za-z0-9_-]{24,}\b"),
    re.compile(r"\bgh[pousr]_[A-Za-z0-9]{30,}\b"),
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
)
SECRET_REF = re.compile(r"\{secret:([^}]+)\}")
ATTENTION = {
    "oauth-no-dcr",
    "oauth-metadata-missing",
    "endpoint-missing",
    "unreachable",
    "tls-error",
    "unexpected-status",
    "bad-response",
}


class FetchResult:
    __slots__ = ("status", "headers", "text", "error")

    def __init__(self, status, headers, text, error):
        self.status = status
        self.headers = headers
        self.text = text
        self.error = error


class UnsafeUrl(ValueError):
    """The URL is not one this checker is willing to contact (fixed reason text only)."""


def _address_ok(address: str) -> bool:
    ip = ipaddress.ip_address(address.split("%", 1)[0])
    mapped = getattr(ip, "ipv4_mapped", None)
    return (mapped or ip).is_global


def validate_url(url: str, allow_loopback: bool = False) -> None:
    """Refuse anything but a public https host. Raises UnsafeUrl.

    Remote-controlled URLs (resource_metadata, authorization_servers) pass through here before
    every fetch. Loopback http is only reachable with allow_loopback (hermetic tests), never by
    default and never from the workflow.
    """
    if not isinstance(url, str):
        raise UnsafeUrl("not a string")
    parts = urllib.parse.urlsplit(url)
    host = parts.hostname
    if not host:
        raise UnsafeUrl("no host (relative or malformed url)")
    host = host.lower().rstrip(".")
    try:
        literal = ipaddress.ip_address(host)
    except ValueError:
        literal = None
    if (
        allow_loopback
        and parts.scheme in ("http", "https")
        and (host == "localhost" or (literal is not None and literal.is_loopback))
    ):
        return
    if parts.scheme != "https":
        raise UnsafeUrl("scheme is not https")
    if literal is not None:
        raise UnsafeUrl("ip literal host")
    if host == "localhost" or host.endswith(".localhost"):
        raise UnsafeUrl("localhost host")
    port = parts.port  # raises ValueError on a malformed port
    try:
        infos = socket.getaddrinfo(host, port or 443, type=socket.SOCK_STREAM)
    except OSError:
        raise UnsafeUrl("host does not resolve") from None
    addresses = {info[4][0] for info in infos}
    if not addresses or not all(_address_ok(address) for address in addresses):
        raise UnsafeUrl("non-global address")


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """A redirect is a new, unvalidated target: refuse it and surface the 3xx instead."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def _build_opener() -> urllib.request.OpenerDirector:
    # Deliberately not build_opener(): no file/ftp/data handlers and no environment proxies.
    opener = urllib.request.OpenerDirector()
    for handler in (
        urllib.request.HTTPHandler(),
        urllib.request.HTTPSHandler(),
        urllib.request.HTTPDefaultErrorHandler(),
        _NoRedirect(),
        urllib.request.HTTPErrorProcessor(),
    ):
        opener.add_handler(handler)
    return opener


def _read_capped(stream, deadline: float) -> str:
    """Read at most MAX_BODY_BYTES, giving up when the total deadline passes."""
    chunks: list[bytes] = []
    size = 0
    # read1 returns what is available instead of blocking until the whole chunk arrives, so the
    # deadline is re-checked between the pieces of a slow-drip body.
    read = getattr(stream, "read1", stream.read)
    while size < MAX_BODY_BYTES:
        if time.monotonic() > deadline:
            raise TimeoutError("total deadline exceeded")
        chunk = read(min(4096, MAX_BODY_BYTES - size))
        if not chunk:
            break
        chunks.append(chunk)
        size += len(chunk)
    return b"".join(chunks).decode("utf-8", "replace")


def _fetch(url: str, timeout: float, body, headers, allow_loopback: bool) -> FetchResult:
    validate_url(url, allow_loopback)
    deadline = time.monotonic() + timeout * TOTAL_DEADLINE_FACTOR
    req = urllib.request.Request(url, data=body, headers=headers or {"User-Agent": USER_AGENT})
    try:
        with _build_opener().open(req, timeout=timeout) as response:
            text = _read_capped(response, deadline)
            return FetchResult(response.status, dict(response.headers.items()), text, None)
    except urllib.error.HTTPError as exc:
        if 300 <= exc.code < 400:
            # The headers (Location) are kept for a caller that validates the next hop itself.
            redirect_headers = dict(exc.headers.items())
            exc.close()
            return FetchResult(0, redirect_headers, "", f"unreachable: redirect refused (HTTP {exc.code})")
        text = _read_capped(exc, deadline)
        response_headers = dict(exc.headers.items())
        exc.close()
        return FetchResult(exc.code, response_headers, text, None)
    except ssl.SSLError as exc:
        return FetchResult(0, {}, "", f"tls-error: {getattr(exc, 'reason', None) or type(exc).__name__}")
    except (socket.timeout, TimeoutError) as exc:
        return FetchResult(0, {}, "", f"timeout: {exc}")
    except urllib.error.URLError as exc:
        return FetchResult(0, {}, "", f"unreachable: {exc.reason}")
    except OSError as exc:
        return FetchResult(0, {}, "", f"unreachable: {exc}")


def fetch(
    url: str,
    timeout: float,
    body: bytes | None = None,
    headers: dict | None = None,
    allow_loopback: bool = False,
) -> FetchResult:
    """One bounded, https-only, non-redirecting request. Never adds an Authorization header.

    Nothing a vendor answers can raise out of here: every failure becomes a FetchResult.
    """
    try:
        return _fetch(url, timeout, body, headers, allow_loopback)
    except UnsafeUrl as exc:
        return FetchResult(0, {}, "", f"unreachable: refused url ({exc})")
    except Exception as exc:  # noqa: BLE001 - one bad vendor reply must not end the run
        return FetchResult(0, {}, "", f"unreachable: {type(exc).__name__}")


def parse_body(result: FetchResult) -> dict | None:
    """Accept a JSON body or the first data: line of an SSE stream."""
    content_type = result.headers.get("Content-Type", "")
    text = result.text.strip()
    if "text/event-stream" in content_type:
        for line in text.splitlines():
            if line.startswith("data:"):
                text = line[5:].strip()
                break
    try:
        parsed = json.loads(text)
    except (ValueError, TypeError):
        return None
    return parsed if isinstance(parsed, dict) else None


def well_known_candidates(base: str, well_known: str) -> list[str]:
    """RFC 9728 / OIDC discovery paths, including the path-aware variant."""
    parts = urllib.parse.urlsplit(base)
    origin = f"{parts.scheme}://{parts.netloc}"
    path = parts.path.rstrip("/")
    candidates = [f"{origin}{path}/.well-known/{well_known}"] if path else []
    candidates.append(f"{origin}/.well-known/{well_known}")
    if path:
        # Some servers use /.well-known/<name>/<path> instead of <path>/.well-known/<name>.
        candidates.append(f"{origin}/.well-known/{well_known}{path}")
    seen: set[str] = set()
    return [url for url in candidates if not (url in seen or seen.add(url))]


def resource_metadata_url(www_authenticate: str) -> str | None:
    match = re.search(r'resource_metadata\s*=\s*"([^"]+)"', www_authenticate, re.IGNORECASE)
    return match.group(1) if match else None


def discover_auth(url: str, challenge: str, timeout: float, allow_loopback: bool = False) -> dict:
    """Try to discover protected-resource + authorization-server metadata."""
    detail: dict = {"dcr": None}
    fetched = 0
    candidates = []
    advertised = resource_metadata_url(challenge)
    if advertised:
        candidates.append(advertised)
    candidates.extend(well_known_candidates(url, "oauth-protected-resource"))
    protected = None
    for candidate in candidates:
        if fetched >= MAX_METADATA_FETCHES:
            break
        fetched += 1
        response = fetch(candidate, timeout, allow_loopback=allow_loopback)
        if response.status == 200:
            protected = parse_body(response)
            if protected:
                break
    if not protected:
        detail["auth"] = "oauth-metadata-missing"
        detail["note"] = "no protected-resource metadata found"
        return detail
    servers = protected.get("authorization_servers") or []
    if not servers or not isinstance(servers[0], str):
        detail["auth"] = "oauth-metadata-missing"
        detail["note"] = "protected-resource metadata has no authorization_servers"
        return detail
    issuer = servers[0]
    detail["issuer"] = issuer
    as_candidates = []
    for well_known in ("oauth-authorization-server", "openid-configuration"):
        as_candidates.extend(well_known_candidates(issuer, well_known))
    for candidate in as_candidates:
        if fetched >= MAX_METADATA_FETCHES:
            break
        fetched += 1
        response = fetch(candidate, timeout, allow_loopback=allow_loopback)
        if response.status == 200:
            metadata = parse_body(response)
            if not metadata:
                continue
            detail["dcr"] = bool(metadata.get("registration_endpoint"))
            detail["auth"] = "oauth-dcr" if detail["dcr"] else "oauth-no-dcr"
            detail["note"] = (
                "dynamic client registration advertised"
                if detail["dcr"]
                else "no registration_endpoint: how does a user sign in?"
            )
            return detail
    detail["auth"] = "oauth-metadata-missing"
    detail["note"] = "authorization-server metadata not discoverable"
    return detail


def check_endpoint(url: str, timeout: float, expects_api_key: bool = False, allow_loopback: bool = False) -> dict:
    """POST an unauthenticated MCP initialize and classify the response."""
    result = {"url": url, "status": "error", "auth": "none", "dcr": None, "note": ""}
    body = json.dumps(INITIALIZE).encode("utf-8")
    response = fetch(url, timeout, body=body, headers=INIT_HEADERS, allow_loopback=allow_loopback)
    if response.error:
        result["status"] = "tls-error" if response.error.startswith("tls-error") else "unreachable"
        result["note"] = response.error
        return result
    code = response.status
    if 200 <= code < 300:
        parsed = parse_body(response)
        result["status"] = "open" if parsed else "bad-response"
        result["note"] = "answers without credentials" if parsed else "2xx without JSON/SSE body"
        return result
    if code in (401, 403):
        result["status"] = "auth-required"
        challenge = response.headers.get("WWW-Authenticate", "")
        discovered = discover_auth(url, challenge, timeout, allow_loopback)
        if discovered.get("auth") == "oauth-metadata-missing" and not challenge and expects_api_key:
            result["status"] = "api-key"
            result["note"] = "401 without OAuth metadata; API key declared in the catalog"
        else:
            result["status"] = discovered["auth"]
            result.update(discovered)
        return result
    if code in (404, 405):
        result["status"] = "endpoint-missing"
        result["note"] = f"HTTP {code}"
        return result
    result["status"] = "unexpected-status"
    result["note"] = f"HTTP {code}"
    return result


def structural_errors(catalog: list) -> list[str]:
    errors: list[str] = []
    ids: set[str] = set()
    for index, entry in enumerate(catalog):
        label = f"entry[{index}]"
        if not isinstance(entry, dict):
            errors.append(f"{label}: not an object")
            continue
        entry_id = entry.get("id")
        if not isinstance(entry_id, str) or not entry_id:
            errors.append(f"{label}: missing id")
        elif entry_id in ids:
            errors.append(f"{entry_id}: duplicate id")
        else:
            ids.add(entry_id)
        label = str(entry_id or label)
        install = entry.get("install")
        if not isinstance(install, dict):
            errors.append(f"{label}: missing install")
            continue
        transport = install.get("transport")
        if transport not in ("http", "stdio"):
            errors.append(f"{label}: unknown transport {transport!r}")
            continue
        declared = {
            secret.get("name")
            for secret in install.get("required_secrets", [])
            if isinstance(secret, dict)
        }
        for secret in declared:
            if not isinstance(secret, str) or not secret.startswith("mcp/"):
                errors.append(f"{label}: secret {secret!r} must start with 'mcp/'")
        for field in ("secret_headers", "secret_env"):
            values = install.get(field) or {}
            if not isinstance(values, dict):
                errors.append(f"{label}: {field} is not an object")
                continue
            for key, value in values.items():
                if not isinstance(value, str):
                    continue
                for name in SECRET_REF.findall(value):
                    if name not in declared:
                        errors.append(f"{label}: {field}.{key} references undeclared secret {name!r}")
        if transport == "http":
            url = install.get("url")
            # Loopback URLs are only useful for the hermetic tests; a shipped catalog is https.
            loopback = isinstance(url, str) and url.startswith(("http://127.0.0.1", "http://localhost"))
            if not isinstance(url, str) or not (url.startswith("https://") or loopback):
                errors.append(f"{label}: http transport needs an https url")
        elif not install.get("command"):
            errors.append(f"{label}: stdio transport needs a command")
        raw = json.dumps(install)
        for pattern in TOKEN_PATTERNS:
            if pattern.search(raw):
                errors.append(f"{label}: install contains a token-looking literal")
    return errors


def _check_entry(entry: dict, timeout: float, allow_loopback: bool) -> dict:
    """One entry can only ever become a result row, never an exception that ends the run."""
    try:
        install = entry["install"]
        return check_endpoint(
            install["url"], timeout, bool(install.get("required_secrets")), allow_loopback=allow_loopback
        )
    except Exception as exc:  # noqa: BLE001
        return {
            "url": None,
            "status": "bad-response",
            "auth": "none",
            "dcr": None,
            "note": f"checker error: {type(exc).__name__}",
        }


def _install_of(entry: dict) -> dict:
    install = entry.get("install")
    return install if isinstance(install, dict) else {}


def run_check(catalog_path: Path, timeout: float, only: str | None = None, allow_loopback: bool = False) -> dict:
    catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
    if not isinstance(catalog, list):
        raise ValueError("catalog must be a JSON list")
    errors = structural_errors(catalog)
    entries = []
    selected = [
        entry for entry in catalog if isinstance(entry, dict) and (only is None or entry.get("id") == only)
    ]
    with ThreadPoolExecutor(max_workers=4) as pool:
        futures = {
            id(entry): pool.submit(_check_entry, entry, timeout, allow_loopback)
            for entry in selected
            if _install_of(entry).get("transport") == "http"
        }
        checked = {key: future.result() for key, future in futures.items()}
    for entry in selected:
        install = _install_of(entry)
        result = {
            "id": entry.get("id"),
            "transport": install.get("transport"),
            "status": "skipped-stdio",
            "auth": "none",
            "dcr": None,
            "note": "stdio entries are checked offline by catalog_contract.rs",
        }
        if install.get("transport") == "http":
            result.update(checked[id(entry)])
        entries.append(result)
    attention = [
        {"id": item["id"], "status": item["status"], "note": item["note"]}
        for item in entries
        if item["status"] in ATTENTION
    ]
    return {
        "checked_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "catalog": str(catalog_path),
        "entries": entries,
        "attention": attention,
        "structural_errors": errors,
    }


def render_table(report: dict) -> str:
    lines = [f"{'id':<16} {'transport':<8} {'status':<22} {'auth':<20} note"]
    for item in report["entries"]:
        lines.append(
            f"{str(item['id']):<16} {str(item['transport']):<8} {str(item['status']):<22} "
            f"{str(item['auth']):<20} {item['note']}"
        )
    for error in report["structural_errors"]:
        lines.append(f"STRUCTURAL ERROR: {error}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--catalog", type=Path, default=Path("crates/mcp-bridge/catalog.json"))
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--only", help="check a single catalog id")
    parser.add_argument("--json", dest="json_path", type=Path, help="write the machine-readable report here")
    parser.add_argument("--strict", action="store_true", help="also fail on vendor/endpoint attention")
    parser.add_argument("--allow-loopback", action="store_true", help=argparse.SUPPRESS)  # hermetic tests only
    args = parser.parse_args(argv)
    try:
        report = run_check(args.catalog, args.timeout, args.only, args.allow_loopback)
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"check_mcp_catalog: internal error: {exc}", file=sys.stderr)
        return 1
    if args.json_path:
        args.json_path.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(render_table(report))
    if report["structural_errors"]:
        return 2
    if args.strict and report["attention"]:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
