#!/usr/bin/env python3
"""Package a native binary, versioned documentation and dependency licenses."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tarfile
import zipfile

root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--platform", required=True, choices=[
    "linux-x86_64", "macos-arm64", "macos-x86_64", "windows-x86_64"])
args = parser.parse_args()
metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--format-version", "1", "--locked"], cwd=root))
package = next(p for p in metadata["packages"] if p["name"] == "ovpnlane" and p["source"] is None)
version = package["version"]
windows = args.platform.startswith("windows-")
executable = "ovpnlane.exe" if windows else "ovpnlane"
binary = Path(metadata["target_directory"]) / "release" / executable
reported = subprocess.check_output([str(binary), "--version"], text=True).strip()
if reported != f"ovpnlane {version}":
    parser.error(f"binary version mismatch: {reported}")
destination = root / "dist"
destination.mkdir(exist_ok=True)
notices = ["OvpnLane third-party license notices",
           "Generated from Cargo.lock. Includes build and development dependencies.",
           "Unmodified crate sources are available at the source URLs listed below.",
           "Native OpenVPN/Asio compatibility patches are included in openvpn-connect-sys.",
           ""]
for dependency in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
    if dependency["source"] is None:
        continue
    name, release = dependency["name"], dependency["version"]
    notices.extend(["=" * 72, f"{name} {release}",
                    f"License expression: {dependency['license'] or 'see license files'}",
                    f"Source: https://crates.io/api/v1/crates/{name}/{release}/download"])
    source = Path(dependency["manifest_path"]).parent
    license_paths = {p for p in source.rglob("*") if p.is_file()
                     and re.match(r"^(licen[cs]e|copying|copyright|notice)([._-]|$)", p.name, re.I)}
    if dependency["license_file"]:
        license_paths.add(source / dependency["license_file"])
    for path in sorted(license_paths):
        if path.is_file():
            notices.extend([f"--- {path.relative_to(source).as_posix()} ---",
                            path.read_text(encoding="utf-8", errors="replace")])
license_bundle = destination / "THIRD_PARTY_LICENSES.txt"
license_bundle.write_text("\n".join(notices), encoding="utf-8")
files = [(binary, executable), (license_bundle, license_bundle.name)]
files += [(root / name, name) for name in ["README.md", "LICENSE", "THIRD_PARTY.md", "CHANGELOG.md"]]
archive = destination / (f"ovpnlane-{version}-{args.platform}" + (".zip" if windows else ".tar.gz"))
if windows:
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as output:
        for source, name in files:
            output.write(source, name)
else:
    with tarfile.open(archive, "w:gz") as output:
        for source, name in files:
            output.add(source, arcname=name)
checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_name(archive.name + ".sha256").write_text(f"{checksum}  {archive.name}\n", encoding="utf-8")
print(archive)
