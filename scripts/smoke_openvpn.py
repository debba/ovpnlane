#!/usr/bin/env python3
"""Real OpenVPN TLS/data-channel negotiation on loopback, without a kernel TUN.

Requires OpenVPN 2.6+ and OpenSSL executables. The server uses `dev null`:
this tests authentication/negotiation, while `cargo test` exercises packet I/O.
All certificates are temporary, generated locally and deleted afterwards.
"""
import argparse
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time


def run(command, cwd):
    result = subprocess.run(command, cwd=cwd, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f"{command[0]} failed:\n{result.stderr}")


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def stop(process):
    if process and process.poll() is None:
        process.send_signal(signal.SIGINT if os.name != "nt" else signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/debug/ovpnlane")
    parser.add_argument("--openvpn", default=shutil.which("openvpn"))
    parser.add_argument("--openssl", default=shutil.which("openssl"))
    parser.add_argument("--transport", choices=["tcp", "udp"], default="tcp")
    parser.add_argument("--control-key", choices=["tls-crypt", "tls-auth"], default="tls-crypt")
    parser.add_argument("--wrong-server-name", action="store_true", help="verify that TLS rejects an unexpected server identity")
    args = parser.parse_args()
    if not args.openvpn or not args.openssl:
        parser.error("provide --openvpn and --openssl executables")
    binary = str(Path(args.binary).resolve())
    with tempfile.TemporaryDirectory(prefix="ovpnlane-test-") as temporary:
        root = Path(temporary)
        run([args.openssl, "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "ca.key", "-out", "ca.crt", "-days", "1", "-subj", "/CN=Proxy test CA", "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign"], root)
        for role, purpose in [("server", "serverAuth"), ("client", "clientAuth")]:
            run([args.openssl, "req", "-newkey", "rsa:2048", "-nodes", "-keyout", f"{role}.key", "-out", f"{role}.csr", "-subj", f"/CN={role}"], root)
            (root / f"{role}.ext").write_text(f"basicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage={purpose}\n")
            run([args.openssl, "x509", "-req", "-in", f"{role}.csr", "-CA", "ca.crt", "-CAkey", "ca.key", "-CAcreateserial", "-out", f"{role}.crt", "-days", "1", "-extfile", f"{role}.ext"], root)
        run([args.openvpn, "--genkey", "secret", "control.key"], root)
        server_key = "tls-auth control.key 0\nauth SHA256" if args.control_key == "tls-auth" else "tls-crypt control.key"
        client_key = "tls-auth control.key 1\nauth SHA256" if args.control_key == "tls-auth" else "tls-crypt control.key"
        vpn_port, socks_port = free_port(), free_port()
        (root / "server.conf").write_text(f'''dev null
tls-server
proto {"tcp-server" if args.transport == "tcp" else "udp"}
local 127.0.0.1
port {vpn_port}
ca ca.crt
cert server.crt
key server.key
dh none
{server_key}
remote-cert-tls client
data-ciphers AES-256-GCM
cipher AES-256-GCM
ifconfig-noexec
route-noexec
push "topology subnet"
push "ifconfig 10.8.0.2 255.255.255.0"
push "route-gateway 10.8.0.1"
push "dhcp-option DNS 10.8.0.1"
push "ping 5"
push "ping-restart 15"
verb 3
''')
        (root / "client.ovpn").write_text(f'''client
dev tun
proto {"tcp-client" if args.transport == "tcp" else "udp"}
remote 127.0.0.1 {vpn_port}
ca ca.crt
cert client.crt
key client.key
{client_key}
remote-cert-tls server
verify-x509-name {"wrong-server" if args.wrong_server_name else "server"} name
data-ciphers AES-256-GCM
nobind
''')
        server = client = None
        try:
            with (root / "server.log").open("w") as server_log, (root / "client.log").open("w") as client_log:
                server = subprocess.Popen([args.openvpn, "--config", "server.conf"], cwd=root, stdout=server_log, stderr=subprocess.STDOUT)
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    if server.poll() is not None:
                        raise RuntimeError((root / "server.log").read_text())
                    server_text = (root / "server.log").read_text()
                    if ("Listening for incoming TCP connection" in server_text
                            or "UDPv4 link local" in server_text):
                        break
                    time.sleep(0.05)
                else:
                    raise RuntimeError("local OpenVPN server did not start")
                run([binary, "--config", str(root / "client.ovpn"), "--check"], root)
                environment = os.environ.copy()
                for name in ("OVPN_USER", "OVPN_PASS", "OVPN_KEY_PASS", "OVPN_RESPONSE"):
                    environment.pop(name, None)
                environment["RUST_LOG"] = "ovpnlane=debug"
                client = subprocess.Popen([binary, "--config", str(root / "client.ovpn"), "--listen", f"127.0.0.1:{socks_port}", "--non-interactive", "--connect-timeout", "10"], cwd=root, stdout=client_log, stderr=subprocess.STDOUT, env=environment)
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    text = (root / "client.log").read_text()
                    if args.wrong_server_name and ("CERT_VERIFY_FAIL" in text or "certificate verification failed" in text.lower()):
                        if "event=CONNECTED" in text:
                            raise RuntimeError("client connected despite the wrong certificate identity")
                        print("PASS: real OpenVPN server rejected for mismatched certificate identity")
                        break
                    if client.poll() is not None:
                        raise RuntimeError(f"client exited:\n{text}\nserver:\n{(root / 'server.log').read_text()}")
                    if 'event=CONNECTED' in text or 'event\x1b[0m\x1b[2m=\x1b[0mCONNECTED' in text:
                        if args.wrong_server_name:
                            raise RuntimeError("client accepted the wrong certificate identity")
                        print(f"PASS: real OpenVPN over {args.transport}, {args.control_key} and AES-256-GCM; userspace tunnel CONNECTED")
                        break
                    time.sleep(0.05)
                else:
                    raise RuntimeError(f"handshake timeout:\n{text}\nserver:\n{(root / 'server.log').read_text()}")
        finally:
            stop(client)
            stop(server)


if __name__ == "__main__":
    main()
