"""Exercise the installer's actual generated wrapper without installing anything."""
import os
from pathlib import Path
import subprocess
import tempfile

source = (Path(__file__).parents[1] / "install-linux.sh").read_text()
function = source[source.index("write_wrapper() {"):source.index("# ------------------------------------------------------------ environment")]
with tempfile.TemporaryDirectory() as temporary:
    root = Path(temporary)
    (root / "bin").mkdir()
    (root / "mocks").mkdir()
    function = function.replace("/usr/local/bin/", str(root) + "/")
    subprocess.run(["bash", "-c", "\n".join([
        "set -eu", f"PREFIX='{root}'", "SVC_USER=praxi", "HARDENING=''",
        f"UNIT_DIR='{root}/units'", f"DATA_DIR='{root}/data'", function,
        f"write_wrapper praxi bot '{root}/must-not-source'",
        f"write_wrapper praxi-node bot-node '{root}/must-not-source'",
    ])], check=True)
    # Mock only privilege routing, never sudo or real service state.
    for name, body in {
        "id": 'echo "$TEST_UID"',
        "runuser": 'echo runuser "$@"',
        "sudo": 'echo sudo "$@"',
    }.items():
        file = root / "mocks" / name
        file.write_text("#!/bin/sh\n" + body + "\n")
        file.chmod(0o755)
    bot = root / "bin/bot"
    bot.write_text('#!/bin/sh\nprintf "maintenance:%s:%s\\n" "$*" "${TEST_SECRET-absent}"\n')
    bot.chmod(0o755)
    def run(command, uid, *args):
        env = dict(os.environ, PATH=f"{root}/mocks:/usr/bin:/bin", TEST_UID=str(uid), TEST_SECRET="fixture")
        return subprocess.check_output(["sh", str(root / command), *args], env=env, text=True)
    assert run("praxi", 0, "self-update", "--check").strip() == "maintenance:self-update --check:absent"
    assert run("praxi", 0, "serve").startswith("runuser -u praxi")
    assert run("praxi", 1000, "self-update").startswith("sudo -u praxi")
    assert run("praxi-node", 0, "self-update").startswith("runuser -u praxi")
print("wrapper: root self-update retains maintenance privilege; serve/node/non-root use service user; no service secrets sourced")
