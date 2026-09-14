#!/usr/bin/env python3
"""Installer contract tests with local release fixtures; no network or real install."""
import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]

class InstallerTests(unittest.TestCase):
    def test_latest_install_and_corrupt_update_preserves_existing_binary(self):
        with tempfile.TemporaryDirectory(prefix="ovpnlane-installer-test-") as temp:
            root = Path(temp)
            mocks = root / "commands"
            mocks.mkdir()
            fixture = root / "release.tar.gz"
            binary = b'#!/bin/sh\nprintf "ovpnlane 9.8.7\\n"\n'
            with tarfile.open(fixture, "w:gz") as archive:
                entry = tarfile.TarInfo("ovpnlane")
                entry.size, entry.mode = len(binary), 0o755
                archive.addfile(entry, io.BytesIO(binary))
            manifest = root / "release.sha256"
            manifest.write_text(hashlib.sha256(fixture.read_bytes()).hexdigest()
                + "  ovpnlane-9.8.7-macos-arm64.tar.gz\n")
            (mocks / "uname").write_text('#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n')
            (mocks / "curl").write_text("""#!/usr/bin/env python3
import os, pathlib, shutil, sys
args = sys.argv[1:]
url = next(a for a in args if a.startswith("https://"))
assert url.startswith("https://github.com/debba/ovpnlane/releases/")
if url.endswith("/latest"):
    print("https://github.com/debba/ovpnlane/releases/tag/v9.8.7", end="")
else:
    assert "/download/v9.8.7/" in url
    root = pathlib.Path(os.environ["RELEASE_FIXTURE"])
    source = root / ("release.sha256" if url.endswith(".sha256") else "release.tar.gz")
    shutil.copyfile(source, args[args.index("-o") + 1])
""")
            for command in mocks.iterdir():
                command.chmod(0o755)
            install = root / "path with spaces" / "bin"
            environment = dict(os.environ, PATH=str(mocks) + os.pathsep + os.environ["PATH"],
                OVPNLANE_INSTALL_DIR=str(install), RELEASE_FIXTURE=str(root))
            environment.pop("OVPNLANE_VERSION", None)
            def run():
                return subprocess.run(["sh", str(ROOT / "install.sh")], env=environment,
                                      capture_output=True, text=True)
            result = run()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual((install / "ovpnlane").read_bytes(), binary)
            original = b"keep this existing binary"
            (install / "ovpnlane").write_bytes(original)
            manifest.write_text("0" * 64 + "  ovpnlane-9.8.7-macos-arm64.tar.gz\n")
            environment["OVPNLANE_VERSION"] = "9.8.7"
            result = run()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("checksum mismatch", result.stderr)
            self.assertEqual((install / "ovpnlane").read_bytes(), original)
            environment["OVPNLANE_VERSION"] = "../invalid"
            self.assertNotEqual(run().returncode, 0)

if __name__ == "__main__":
    unittest.main()
