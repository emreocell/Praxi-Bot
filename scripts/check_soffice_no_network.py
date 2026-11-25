#!/usr/bin/env python3
"""REQUIRED MANUAL CHECK before wiring doc_convert / doc_preview into the daemon.

Runs the REAL LibreOffice (skipped, exit 0, when `soffice` is not installed) on hostile
documents WITHOUT the Rust vetting and WITHOUT the network sandbox, with the same
throw-away profile and flags the tool uses, and asserts that a local HTTP listener on
127.0.0.1 gets ZERO requests:

  * a docx with a linked image (TargetMode=External) and an external attached template
  * an xlsx with WEBSERVICE() pointing at the listener
  * an ods with a WEBSERVICE() formula

It also checks that `--safe-mode` kept the seeded `registrymodifications.xcu` (the
reviewer could not confirm the key names or that the profile survives safe mode).

Usage:  python scripts/check_soffice_no_network.py [path-to-soffice]
Exit:   0 = zero hits (or skipped), 1 = the listener was contacted / profile was reset.
The proxy settings in the profile point at a closed port, so a hit here means a request
that bypassed them: the Rust vetting plus the Linux network sandbox are then the only
barrier, and the tool must not be wired for untrusted documents on a host without one.
"""

import http.server
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import zipfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
CONVERT_RS = ROOT / "crates" / "doc-tools" / "src" / "convert.rs"

hits = []


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802
        hits.append(self.path)
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"1")

    do_POST = do_HEAD = do_GET

    def log_message(self, *args):
        pass


def find_soffice(arg):
    if arg:
        return arg
    return shutil.which("soffice") or shutil.which("libreoffice")


def seeded_xcu():
    text = CONVERT_RS.read_text(encoding="utf-8")
    m = re.search(r'const XCU: &str = r#"(.*?)"#;', text, re.S)
    if not m:
        sys.exit("cannot find the XCU profile in convert.rs")
    return m.group(1)


def make_docx(path, base):
    rels = (
        '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        f'<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="{base}/linked.png" TargetMode="External"/>'
        "</Relationships>"
    )
    settings_rels = (
        '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        f'<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/attachedTemplate" Target="{base}/template.dotm" TargetMode="External"/>'
        "</Relationships>"
    )
    body = (
        '<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" '
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" '
        'xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" '
        'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" '
        'xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><w:body><w:p><w:r><w:t>hello</w:t></w:r>'
        '<w:r><w:drawing><wp:inline><wp:extent cx="990000" cy="790000"/><wp:docPr id="1" name="p"/>'
        '<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic>'
        '<pic:nvPicPr><pic:cNvPr id="0" name="p"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:link="rId1"/></pic:blipFill>'
        '<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="990000" cy="790000"/></a:xfrm></pic:spPr></pic:pic>'
        "</a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p></w:body></w:document>"
    )
    settings = (
        '<?xml version="1.0"?><w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" '
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:attachedTemplate r:id="rId1"/></w:settings>'
    )
    ct = (
        '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/>'
        '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>'
        '<Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/>'
        "</Types>"
    )
    root_rels = (
        '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>'
        "</Relationships>"
    )
    doc_rels = (
        '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        '<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/>'
        + rels.split("<Relationships")[1].split(">", 1)[1].replace("</Relationships>", "")
        + "</Relationships>"
    )
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", ct)
        z.writestr("_rels/.rels", root_rels)
        z.writestr("word/document.xml", body)
        z.writestr("word/settings.xml", settings)
        z.writestr("word/_rels/document.xml.rels", doc_rels)
        z.writestr("word/_rels/settings.xml.rels", settings_rels)


def make_xlsx(path, base):
    ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    z = zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED)
    z.writestr(
        "[Content_Types].xml",
        '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/>'
        '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
        '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
        "</Types>",
    )
    z.writestr(
        "_rels/.rels",
        '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>',
    )
    z.writestr(
        "xl/workbook.xml",
        f'<?xml version="1.0"?><workbook xmlns="{ns}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
        '<sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>',
    )
    z.writestr(
        "xl/_rels/workbook.xml.rels",
        '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>',
    )
    z.writestr(
        "xl/worksheets/sheet1.xml",
        f'<?xml version="1.0"?><worksheet xmlns="{ns}"><sheetData><row r="1">'
        f'<c r="A1"><f>WEBSERVICE("{base}/webservice")</f></c>'
        f'<c r="A2"><f>_xlfn.WEBSERVICE("{base}/webservice2")</f></c></row></sheetData></worksheet>',
    )
    z.close()


def make_ods(path, base):
    ns = (
        'xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
        'xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" '
        'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"'
    )
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("mimetype", "application/vnd.oasis.opendocument.spreadsheet", zipfile.ZIP_STORED)
        z.writestr(
            "META-INF/manifest.xml",
            '<?xml version="1.0"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2">'
            '<manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/>'
            '<manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>',
        )
        z.writestr(
            "content.xml",
            f'<?xml version="1.0"?><office:document-content {ns} office:version="1.2"><office:body><office:spreadsheet>'
            '<table:table table:name="S"><table:table-row>'
            f'<table:table-cell table:formula="of:=WEBSERVICE(&quot;{base}/ods&quot;)"/>'
            "</table:table-row></table:table></office:spreadsheet></office:body></office:document-content>",
        )


def run(soffice, doc, outdir, profile):
    cmd = [
        soffice,
        f"-env:UserInstallation={profile.as_uri()}",
        "--headless",
        "--safe-mode",
        "--norestore",
        "--nolockcheck",
        "--nodefault",
        "--nologo",
        "--convert-to",
        "pdf",
        "--outdir",
        str(outdir),
        str(doc),
    ]
    try:
        subprocess.run(cmd, timeout=180, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except subprocess.TimeoutExpired:
        print(f"  (timeout on {doc.name}; hits so far still count)")


def main():
    soffice = find_soffice(sys.argv[1] if len(sys.argv) > 1 else None)
    if not soffice:
        print("SKIPPED: soffice is not installed (install the office pack to run this check)")
        return 0
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_address[1]}"
    failed = False
    with tempfile.TemporaryDirectory(prefix="praxi-soffice-check-") as tmp:
        tmp = pathlib.Path(tmp)
        docs = [("linked.docx", make_docx), ("webservice.xlsx", make_xlsx), ("webservice.ods", make_ods)]
        for name, maker in docs:
            doc = tmp / name
            maker(doc, base)
            profile = tmp / ("profile-" + name)
            (profile / "user").mkdir(parents=True)
            (profile / "user" / "registrymodifications.xcu").write_text(seeded_xcu(), encoding="utf-8")
            out = tmp / ("out-" + name)
            out.mkdir()
            before = len(hits)
            run(soffice, doc, out, profile)
            made = any(out.iterdir())
            print(f"{name}: converted={made} listener_hits={len(hits) - before}")
            if len(hits) > before:
                failed = True
            kept = (profile / "user" / "registrymodifications.xcu").read_text(encoding="utf-8", errors="replace")
            if "MacroSecurityLevel" not in kept:
                print(f"  FAIL: --safe-mode reset or ignored the seeded profile for {name}")
                failed = True
    server.shutdown()
    if hits:
        print(f"FAIL: LibreOffice contacted the listener {len(hits)} time(s): {hits}")
        return 1
    if failed:
        return 1
    print("OK: zero requests reached the local listener and the seeded profile survived")
    return 0


if __name__ == "__main__":
    sys.exit(main())
