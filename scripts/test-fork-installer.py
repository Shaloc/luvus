#!/usr/bin/env python3
"""Exercise install.sh offline, only inside a private fixture under target/.

Optional argv[1] is a checkout binary for a real installed-runtime smoke.
No production configuration, binary, server, or network is accessed.
"""
import hashlib
import io
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile


def main():
    repo = Path(__file__).resolve().parent.parent
    root = Path(tempfile.mkdtemp(prefix="installer-smoke-", dir=repo / "target"))
    for directory in ("bin", "downloads", "install", "tmp", "state"):
        (root / directory).mkdir()
    payload = b'#!/bin/sh\n[ "$*" = "--version --remote-session-protocol" ] || exit 90\nprintf "luvus 1.0.99 remote-session=2 transport=9\\n"\n'
    if len(sys.argv) > 1:
        binary = Path(sys.argv[1]).resolve()
        assert binary.is_relative_to(repo / "target"), "only test a checkout build"
        payload = binary.read_bytes()
    package = "luvus-1.0.99-0123456789ab"
    for target in ("x86_64-unknown-linux-musl", "aarch64-apple-darwin"):
        archive = root / "downloads" / f"luvus-{target}.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            info = tarfile.TarInfo(f"{package}-{target}/luvus")
            info.mode, info.size = 0o755, len(payload)
            tar.addfile(info, io.BytesIO(payload))
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        archive.with_suffix(".gz.sha256").write_text(f"{digest}  {archive.name}\n")
    helpers = {
        "uname": '#!/bin/sh\ncase "$1" in -s) echo "$FIXTURE_OS" ;; -m) echo "$FIXTURE_ARCH" ;; *) exit 99 ;; esac\n',
        "curl": '''#!/bin/sh
printf '%s\\n' "$*" >> "$FIXTURE_ROOT/requests"
if [ "$1" = -fsSL ] && [ "$2" = https://api.github.com/repos/Shaloc/luvus/releases/latest ]; then
  printf '{"tag_name":"fork-1.0.99-0123456"}\\n'
elif [ "$1" = -fsSL ] && [ "$2" = -o ]; then
  case "$4" in
    https://github.com/Shaloc/luvus/releases/download/fork-1.0.99-0123456/luvus-*)
      cp "$FIXTURE_ROOT/downloads/${4##*/}" "$3" ;;
    *) exit 98 ;;
  esac
else exit 97
fi
''',
    }
    for name, source in helpers.items():
        path = root / "bin" / name
        path.write_text(source)
        path.chmod(0o755)
    env = {k: v for k, v in os.environ.items() if not k.startswith("LUVUS_")}
    env.update(PATH=f"{root}/bin:{env['PATH']}", LUVUS_INSTALL_DIR=str(root / "install"),
               LUVUS_HOME=str(root / "state"), TMPDIR=str(root / "tmp"),
               FIXTURE_ROOT=str(root), FIXTURE_OS="Linux", FIXTURE_ARCH="x86_64")
    installed = root / "install/luvus"

    def run(ok=True, **extra):
        result = subprocess.run(["sh", str(repo / "install.sh")], env=dict(env, **extra),
                                text=True, capture_output=True, timeout=30)
        assert (result.returncode == 0) == ok, result.stdout + result.stderr
        assert not list((root / "tmp").iterdir()), "download fixture leaked"
        return result.stdout + result.stderr

    installed.write_bytes(b"old binary")
    old_inode = installed.stat().st_ino
    assert "No servers were restarted" in run()
    assert installed.read_bytes() == payload and installed.stat().st_ino != old_inode
    assert any(p.read_bytes() == b"old binary" for p in (root / "install").glob(".luvus-update.*/luvus.previous"))
    assert "installed" in run(LUVUS_VERSION="fork-1.0.99-0123456").lower()
    assert installed.read_bytes() == payload
    if len(sys.argv) == 1:
        run(FIXTURE_OS="Darwin", FIXTURE_ARCH="arm64")
    checksum = root / "downloads/luvus-x86_64-unknown-linux-musl.tar.gz.sha256"
    checksum.write_text("0" * 64 + "  ignored-path\n")
    assert "SHA-256 mismatch" in run(ok=False)
    assert installed.read_bytes() == payload
    assert "unsupported" in run(ok=False, FIXTURE_ARCH="aarch64")
    assert "invalid release tag" in run(ok=False, LUVUS_VERSION="../../bad")
    assert "expected a fork release tag" in run(ok=False, LUVUS_VERSION="v0.13.4")
    # A missing checksum or failed network read must not replace the installation.
    checksum.unlink()
    assert "checksum download failed" in run(ok=False)
    assert installed.read_bytes() == payload
    archive = checksum.with_suffix("")
    checksum.write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + "\n")
    installed.unlink()
    installed.symlink_to(root / "sentinel")
    (root / "sentinel").write_text("keep")
    assert "symlink" in run(ok=False)
    assert (root / "sentinel").read_text() == "keep"
    requests = (root / "requests").read_text()
    assert "RizRiyz" not in requests and "luvus.dev" not in requests
    print(f"PASS: fork installer, pinned update, backup, atomic rename, checksums, platform/tag/symlink rejection ({root})")


if __name__ == "__main__":
    main()
