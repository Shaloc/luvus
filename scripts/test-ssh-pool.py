#!/usr/bin/env python3
"""Linux real loopback OpenSSH acceptance for the managed display pool.

Uses private keys, sshd configuration and isolated Luvus homes under target/.
Does not read or change production SSH/Luvus configuration or restart services.
Requires /usr/sbin/sshd, ssh-keygen and passwordless sudo to launch only the
private loopback sshd. All subprocesses and masters created here are cleaned up.
"""
import json
import os
from pathlib import Path
import pwd
import re
import socket
import subprocess
import sys
import tempfile
import time

repo = Path(__file__).resolve().parent.parent
root = Path(tempfile.mkdtemp(prefix="ssh-pool-real-", dir=repo / "target"))
(root / ".isolated-sshd").write_text(str(root))
root.chmod(0o700)
user = pwd.getpwuid(os.getuid()).pw_name
for name in ("host", "client"):
    subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / name)], check=True)
with socket.socket() as listener:
    listener.bind(("127.0.0.1", 0))
    port = listener.getsockname()[1]
hostkey = (root / "host.pub").read_text().split()
(root / "known_hosts").write_text(f"[127.0.0.1]:{port} {hostkey[0]} {hostkey[1]}\n")
(root / "ssh_config").write_text(f'''Host fake-dev
  HostName 127.0.0.1
  Port {port}
  User {user}
  IdentityFile {root / 'client'}
  IdentitiesOnly yes
  UserKnownHostsFile {root / 'known_hosts'}
  GlobalKnownHostsFile /dev/null
  StrictHostKeyChecking yes
  ForwardAgent no
''')
(root / "sshd_config").write_text(f'''ListenAddress 127.0.0.1
Port {port}
HostKey {root / 'host'}
PidFile {root / 'sshd.pid'}
AuthorizedKeysFile {root / 'client.pub'}
AllowUsers {user}
PubkeyAuthentication yes
PasswordAuthentication no
ChallengeResponseAuthentication no
# GitHub's runner account has a locked password. PAM account/session handling
# permits its public-key login without unlocking or changing that account.
UsePAM yes
StrictModes yes
MaxStartups 10:30:10
MaxSessions 10
LogLevel VERBOSE
AcceptEnv LUVUS_SMOKE_ROOT LUVUS_SMOKE_BINARY LUVUS_SMOKE_REMOTE_BINARY
ForceCommand {sys.executable} {repo / 'scripts/test-remote-sessions.py'} --isolated-ssh-bridge
''')
log = (root / "sshd.log").open("w")
server = subprocess.Popen(["sudo", "-n", "/usr/sbin/sshd", "-D", "-e", "-f", str(root / "sshd_config")], stdout=log, stderr=log)
result = None
smoke = None
try:
    deadline = time.monotonic() + 5
    while not (root / "sshd.pid").exists():
        if server.poll() is not None or time.monotonic() > deadline:
            raise AssertionError((root / "sshd.log").read_text())
        time.sleep(0.05)
    env = dict(os.environ, LUVUS_TEST_SSH_POOL_CONFIG=str(root / "ssh_config"))
    result = subprocess.run([sys.executable, str(repo / "scripts/test-remote-sessions.py"),
                             *sys.argv[1:], "--ssh-admission-only"], env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=100)
    (root / "smoke.log").write_text(result.stdout)
    print(result.stdout, flush=True)
    matches = re.findall(r"Smoke-test evidence: (.+)", result.stdout)
    if matches:
        smoke = Path(matches[-1]).resolve()
        assert smoke.parent == repo / "target" and smoke.name.startswith("remote-smoke-")
    result.check_returncode()
    commands = [json.loads(line)["args"] for line in (smoke / "ssh-pool-commands").read_text().splitlines()]
    paths = [args[args.index("-S") + 1] for args in commands]
    shared_display = "use 1 display bridge(s)" in result.stdout
    initial_channels = 1 if shared_display else 16
    counts = {path: paths[:initial_channels].count(path) for path in set(paths)}
    assert len(paths) == initial_channels + 1, paths
    assert sorted(counts.values()) == ([1] if shared_display else [8, 8]), counts
    assert paths[-1] == paths[0], "retry should reuse the released channel slot"
    # One public-key authentication per master, including after channel retry.
    ssh_log = (root / "sshd.log").read_text()
    authentications = len(re.findall(r"Accepted publickey for", ssh_log))
    expected_auth = 1 if shared_display else 2
    assert authentications == expected_auth, f"expected {expected_auth} SSH transports, got {authentications}"
    assert "no more sessions" not in ssh_log.lower()
    print(f"PASS: 16 workspaces use {initial_channels} display channels and {expected_auth} authenticated SSH connections; MaxStartups=10 and MaxSessions=10", flush=True)
finally:
    if result is None or result.returncode:
        print("Isolated sshd diagnostics:\n" + (root / "sshd.log").read_text(), flush=True)
    if smoke and (smoke / "ssh-pool-commands").exists():
        commands = [json.loads(line)["args"] for line in (smoke / "ssh-pool-commands").read_text().splitlines()]
        paths = {args[args.index("-S") + 1] for args in commands if "-S" in args}
        for path in paths:
            subprocess.run(["/usr/bin/ssh", "-F", str(root / "ssh_config"), "-S", path, "-O", "exit", "fake-dev"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
    if (root / "sshd.pid").exists():
        pid = int((root / "sshd.pid").read_text())
        # Validate the unique fixture config before signalling this private daemon.
        command = Path(f"/proc/{pid}/cmdline").read_bytes()
        assert str(root / "sshd_config").encode() in command
        subprocess.run(["sudo", "-n", "kill", "-TERM", str(pid)], check=True)
    server.wait(timeout=5)
    log.close()
    print("Real SSH evidence:", root, flush=True)
