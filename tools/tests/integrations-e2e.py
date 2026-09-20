#!/usr/bin/env python3
"""Host product acceptance using real confd/plugin processes and a fake TV.

Only APK extraction is mocked on the host; native signature/repository checks
are exercised separately by tools/integrations. Never contacts a real device.
Build couch-confd and couch-plugin-echo in their respective workspaces first.

With --echo-pair, and only then, this runs the protocol 3 pairing leg instead:
a real couch-plugin-echo-pair package paired through the daemon's own routes
over HTTP. Both binaries have to be built with the preview feature, which no
shipped build enables:

    cargo build --manifest-path clients/Cargo.toml -p couch-echo \
        --bin couch-plugin-echo-pair --features couch-echo/protocol-3-preview
    cargo build --manifest-path daemon/Cargo.toml -p couch-confd \
        --features couch-plugin/protocol-3-preview
"""
import argparse
import json
from pathlib import Path
import shutil
import socket
import socketserver
import struct
import subprocess
import sys
import tempfile
import tarfile
import threading
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]


class Television(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.commands = []
        self.connections = 0


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.server.connections += 1
        while line := self.rfile.readline():
            command = line.decode().strip()
            self.server.commands.append(command)
            if command == "GET STATUS":
                reply = b"STATUS power=on;mute=off;volume=30;input=hdmi1\n"
            elif command == "LIST INPUTS":
                reply = b"INPUT hdmi1 Blu-ray\nINPUT hdmi2 Console\nEND\n"
            elif command.startswith("CMD "):
                reply = b"OK\n"
            else:
                raise AssertionError(command)
            self.wfile.write(reply)
            self.wfile.flush()


def api(base, method, path, data=None, expected=200):
    request = urllib.request.Request(base + path, method=method,
        data=None if data is None else json.dumps(data).encode(),
        headers={"Content-Type": "application/json"})
    try:
        response = urllib.request.urlopen(request, timeout=20)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        body = response.read()
        assert response.code == expected, (method, path, response.code, body)
        return json.loads(body)


def panel(path, connection, request):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(15)
        stream.connect(str(path))
        body = json.dumps({"connection_id": connection, "request": request}).encode()
        stream.sendall(struct.pack(">I", len(body)) + body)
        def read(n):
            result = b""
            while len(result) < n:
                part = stream.recv(n - len(result))
                assert part, "bridge closed before completing reply"
                result += part
            return result
        return json.loads(read(struct.unpack(">I", read(4))[0]))


class PairingTelevision(socketserver.ThreadingTCPServer):
    """The fake set the pairing fixture speaks to.

    -> PAIR button   <- PROMPT button / WAIT / PAIRED {...}
    -> PAIR code     <- PROMPT code 4 digits
    -> PAIR code NNNN<- PAIRED {...} or ERR wrong code
    -> PAIR cancel   <- OK
    """

    allow_reuse_address = True
    daemon_threads = True

    def __init__(self):
        super().__init__(("127.0.0.1", 0), PairingHandler)
        self.commands = []
        self.connections = 0
        self.key = "e2e-0001"


class PairingHandler(socketserver.StreamRequestHandler):
    def handle(self):
        self.server.connections += 1
        asked = 0
        while line := self.rfile.readline():
            command = line.decode().strip()
            self.server.commands.append(command)
            paired = f'PAIRED {{"key":"{self.server.key}"}}\n'.encode()
            if command == "GET STATUS":
                reply = b"STATUS power=on;mute=off;volume=30;input=hdmi1\n"
            elif command.startswith("CMD "):
                reply = b"OK\n"
            elif command == "PAIR cancel":
                reply = b"OK\n"
            elif command in ("PAIR button", "PAIR approve"):
                # Prompt, then one poll that changes nothing, then the key.
                asked += 1
                word = command.split()[1]
                reply = [f"PROMPT {word}\n".encode(), b"WAIT\n", paired][min(asked - 1, 2)]
            elif command == "PAIR code":
                reply = b"PROMPT code 4 digits\n"
            elif command.startswith("PAIR code "):
                reply = paired if command == "PAIR code 0417" else b"ERR wrong code\n"
            else:
                raise AssertionError(command)
            self.wfile.write(reply)
            self.wfile.flush()


def daemon(confd, home, log):
    """Start a --no-auth daemon on a free port and wait for it to answer."""
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    process = subprocess.Popen([str(confd), "--addr", f"127.0.0.1:{port}",
        "--config", str(home / "config.json"), "--pin-file", str(home / "pin"),
        "--no-auth"], stdout=log, stderr=log)
    for _ in range(200):
        try:
            api(base, "GET", "/api/health")
            return process, base
        except (OSError, AssertionError):
            if process.poll() is not None:
                log.seek(0)
                raise AssertionError(log.read())
            time.sleep(0.05)
    process.kill()
    raise AssertionError("daemon did not start")


def install(confd, home, executable, manifest, package_id):
    """Sideload one package built from this tree into a fresh store."""
    payload = home / "payload" / "usr/lib/couch/integrations" / package_id
    (payload / "bin").mkdir(parents=True)
    shutil.copy2(executable, payload / "bin" / executable.name)
    shutil.copy2(manifest, payload / "manifest.json")
    fake_apk = home / "fixture-apk"
    fake_apk.write_text(f"#!{sys.executable}\n"
        "import pathlib,shutil,sys\n"
        "args=sys.argv[1:]\n"
        "root=pathlib.Path(args[args.index('--root')+1])\n"
        f"shutil.copytree({str(home / 'payload')!r},root,dirs_exist_ok=True)\n")
    fake_apk.chmod(0o755)
    package = home / "fixture.apk"
    with tarfile.open(package, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
        archive.add(payload, arcname=f"usr/lib/couch/integrations/{package_id}")
    subprocess.run([str(confd), "integrations", "--root", str(home / "integrations"),
                    "--apk", str(fake_apk), "install-sideload", str(package)],
                   check=True, capture_output=True)


def run_pairing(confd, echo_pair, keep):
    """A real protocol 3 package, paired through the daemon's own routes.

    Everything here needs the preview feature, which no shipped build enables:
    with it off the manifest below is refused as needing a newer Couch and
    every route answers "This integration does not pair".
    """
    manifest = ROOT / "clients/couch-echo/tests/fixtures/plugin-pair-v3.json"
    with tempfile.TemporaryDirectory(prefix="couch-pair-e2e-", dir="/tmp") as temporary:
        home = Path(temporary)
        install(confd, home, echo_pair, manifest, "echo-pair")
        with PairingTelevision() as tv:
            threading.Thread(target=tv.serve_forever, daemon=True).start()
            with (home / "daemon.log").open("w+") as log:
                process, base = daemon(confd, home, log)
                try:
                    pairing_flow(base, home, tv)
                finally:
                    process.terminate()
                    try: process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                    tv.shutdown()
                    if keep:
                        keep.mkdir(parents=True, exist_ok=True)
                        shutil.copy2(home/"daemon.log", keep/"integration-pair-e2e.log")
                # Nothing the daemon said, in all of that, was a key. Read
                # once the daemon has stopped, so everything it wrote is out.
                said = (home / "daemon.log").read_text(errors="replace")
                for secret in ("e2e-0001", "rotated-0002"):
                    assert secret not in said, f"{secret} reached the log"
                print("Pairing product flow passed: unpaired without a child, all three "
                      "route replies, the key on disk at 0600 in a 0700 folder, never over "
                      "HTTP and never in the log, a rotated key, forget, a wrong code, a "
                      "cancel, and the panel refused.")


def pairing_flow(base, home, tv):
    catalog = api(base, "GET", "/api/integrations")
    assert [p["id"] for p in catalog["integrations"]] == ["echo-pair"], catalog
    config = api(base, "POST", "/api/connections", {"name": "Paired TV", "provider": {
        "kind": "plugin", "id": "echo-pair", "label": "", "capabilities": []}})
    connection = config["connections"][0]["id"]
    scoped = f"/api/connections/{connection}/plugin"
    stored = home / "connections" / connection
    key_file = stored / "plugin-credential.json"
    summary_file = stored / "plugin-pairing.json"

    settings = api(base, "POST", scoped + "/settings", {
        "host": "127.0.0.1", "port": tv.server_address[1], "mode": "button"})
    assert settings["configured"], settings
    # Nothing is paired, and the package says so about itself.
    view = api(base, "GET", scoped + "/settings")
    assert view["paired"] is False, view
    assert view["pairing"] == {"required": True}, view
    assert "summary" not in view, view

    # A connection that must be paired is refused without a child at all.
    before = tv.connections
    refused = api(base, "GET", scoped + "/status", expected=409)
    assert refused["code"] == "unpaired", refused
    assert tv.connections == before, "no package child may be started"
    config_before = (home / "config.json").read_bytes()

    # Pair: press the button, one poll that changes nothing, then the key.
    started = api(base, "POST", scoped + "/pair", {"settings": {}})
    session = started["session"]
    assert len(session) == 32 and all(c in "0123456789abcdef" for c in session), started
    assert started["step"] == {"step": "waiting",
        "prompt": {"kind": "press_button", "message": "The PAIR button is under the screen"},
        "poll_after_ms": 500}, started
    assert 0 < started["expires_in"] <= 120, started
    polled = api(base, "POST", f"{scoped}/pair/{session}", {})
    assert polled["step"]["step"] == "waiting", polled
    done = api(base, "POST", f"{scoped}/pair/{session}", {})["step"]
    assert done["step"] == "done", done
    assert done["summary"] == "Paired with 127.0.0.1", done
    assert done["settings"]["settings"]["host"] == "127.0.0.1", done
    assert "e2e-0001" not in json.dumps(done), done

    # The key is on the remote, at 0600 in a folder only root may look in,
    # and in nothing a browser can read.
    assert json.loads(key_file.read_text()) == {"key": "e2e-0001"}
    assert key_file.stat().st_mode & 0o777 == 0o600
    assert stored.stat().st_mode & 0o777 == 0o700, oct(stored.stat().st_mode)
    assert json.loads(summary_file.read_text())["summary"] == "Paired with 127.0.0.1"
    assert json.loads(summary_file.read_text())["package"] == "echo-pair"
    assert (home / "config.json").read_bytes() == config_before, "T3 writes nothing to config"
    view = api(base, "GET", scoped + "/settings")
    assert view["paired"] is True and view["summary"] == "Paired with 127.0.0.1", view
    for elsewhere in (view, api(base, "GET", "/api/config"),
                      api(base, "GET", "/api/integrations")):
        assert "e2e-0001" not in json.dumps(elsewhere), elsewhere
    # The session is over.
    api(base, "POST", f"{scoped}/pair/{session}", {}, expected=404)

    # With the key, the set answers - and says nothing about the key.
    status = api(base, "GET", scoped + "/status")
    assert status["volume"] == 30
    assert "e2e-0001" not in json.dumps(status), status

    # A key the set issues while answering replaces the one on disk, under
    # the connection's lock, before the reading comes back.
    api(base, "POST", scoped + "/settings", {"rotate": True})
    status = api(base, "GET", scoped + "/status")
    assert status["volume"] == 30
    assert "rotated-0002" not in json.dumps(status), status
    assert json.loads(key_file.read_text()) == {"key": "rotated-0002"}
    # The child that answers next is configured with the new key, which is
    # the whole point of writing it: the set has forgotten the old one.
    assert api(base, "GET", scoped + "/status")["volume"] == 30

    # Forgetting it removes Couch's copy and says nothing to the set.
    said = len(tv.commands)
    assert api(base, "DELETE", scoped + "/credential") == {"paired": False}
    assert not key_file.exists() and not summary_file.exists()
    assert tv.commands[said:] == [], "forget must not talk to the device"
    assert api(base, "GET", scoped + "/status", expected=409)["code"] == "unpaired"

    # A code the prompt did not ask for never reaches the package.
    api(base, "POST", scoped + "/settings", {"mode": "code", "rotate": False})
    started = api(base, "POST", scoped + "/pair", {"settings": {}})
    session = started["session"]
    assert started["step"] == {"step": "waiting",
        "prompt": {"kind": "enter_code", "message": "The code is on the screen",
                   "length": 4, "alphabet": "digits"},
        "poll_after_ms": 0}, started
    said = len(tv.commands)
    for wrong in ["04a7", "041", "04177"]:
        api(base, "POST", f"{scoped}/pair/{session}",
            {"input": {"kind": "code", "code": wrong}}, expected=400)
    assert tv.commands[said:] == [], "a mistyped code costs no round trip"
    failed = api(base, "POST", f"{scoped}/pair/{session}",
                 {"input": {"kind": "code", "code": "9999"}})["step"]
    assert failed == {"step": "failed", "reason": "wrong_code",
                      "message": "That was not the code on the screen"}, failed
    assert not key_file.exists(), "a failed pairing stores nothing"

    # A cancelled one stores nothing either, and the set is told.
    started = api(base, "POST", scoped + "/pair", {"settings": {}})
    session = started["session"]
    said = len(tv.commands)
    assert api(base, "DELETE", f"{scoped}/pair/{session}") == {"cancelled": True}
    for _ in range(50):
        if "PAIR cancel" in tv.commands[said:]:
            break
        time.sleep(0.02)
    assert "PAIR cancel" in tv.commands[said:], tv.commands[said:]
    assert not key_file.exists()
    api(base, "POST", f"{scoped}/pair/{session}", {}, expected=404)

    # The panel's socket reaches none of this: pairing is not one of the four
    # things `execute` may send.
    for request in [{"method": "pair_start", "settings": {}},
                    {"method": "pair_continue", "session": "p1"},
                    {"method": "pair_cancel", "session": "p1"},
                    {"method": "configure", "settings": {}}]:
        assert panel(home / "plugin.sock", connection, request) == {
            "type": "error", "code": "unsupported"}, request


def run(confd, echo, keep):
    # macOS's Unix socket paths are short; keep fixtures below /tmp.
    with tempfile.TemporaryDirectory(prefix="couch-plugin-e2e-", dir="/tmp") as temporary:
        home = Path(temporary)
        payload = home / "payload" / "usr/lib/couch/integrations/echo"
        (payload / "bin").mkdir(parents=True)
        shutil.copy2(echo, payload / "bin/couch-plugin-echo")
        shutil.copy2(ROOT / "clients/couch-echo/plugin.json", payload / "manifest.json")
        fake_apk = home / "fixture-apk"
        fake_apk.write_text(f"#!{sys.executable}\n"
            "import pathlib,shutil,sys\n"
            "args=sys.argv[1:]\n"
            "assert '--no-network' in args and '--no-scripts' in args\n"
            "assert '--allow-untrusted' not in args\n"
            "root=pathlib.Path(args[args.index('--root')+1])\n"
            f"shutil.copytree({str(home / 'payload')!r},root,dirs_exist_ok=True)\n")
        fake_apk.chmod(0o755)
        package = home / "fixture.apk"
        with tarfile.open(package, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
            archive.add(payload, arcname="usr/lib/couch/integrations/echo")
        command = [str(confd), "integrations", "--root", str(home / "integrations"),
                   "--apk", str(fake_apk), "install-sideload", str(package)]
        subprocess.run(command, check=True, capture_output=True)
        with Television() as tv:
            threading.Thread(target=tv.serve_forever, daemon=True).start()
            with socket.socket() as reservation:
                reservation.bind(("127.0.0.1", 0))
                port = reservation.getsockname()[1]
            base = f"http://127.0.0.1:{port}"
            with (home / "daemon.log").open("w+") as log:
                process = subprocess.Popen([str(confd), "--addr", f"127.0.0.1:{port}",
                    "--config", str(home / "config.json"), "--pin-file", str(home / "pin"),
                    "--no-auth"], stdout=log, stderr=log)
                try:
                    for _ in range(100):
                        try:
                            api(base, "GET", "/api/health")
                            break
                        except (OSError, AssertionError):
                            if process.poll() is not None:
                                log.seek(0)
                                raise AssertionError(log.read())
                            time.sleep(0.05)
                    else:
                        raise AssertionError("daemon did not start")
                    catalog = api(base, "GET", "/api/integrations")
                    assert [p["id"] for p in catalog["integrations"]] == ["echo"]
                    config = api(base, "POST", "/api/connections", {"name":"Example TV", "provider":{
                        "kind":"plugin","id":"echo","label":"forged", "capabilities":[]}})
                    connection = config["connections"][0]
                    assert connection["provider"]["label"] == "Echo TV (example)"
                    assert connection["provider"]["capabilities"]
                    scoped = f"/api/connections/{connection['id']}/plugin"
                    settings = api(base, "POST", scoped + "/settings", {
                        "host":"127.0.0.1","port":tv.server_address[1],"token":"PRIVATE-TOKEN"})
                    assert settings["configured"] and settings["secrets"] == ["token"]
                    assert "PRIVATE-TOKEN" not in json.dumps(settings)
                    assert tv.connections == 0, "configure must work with an offline device"
                    api(base,"POST",scoped+"/settings",{"host":"bad host"},expected=400)
                    api(base,"POST",scoped+"/settings",{"token":""})
                    assert api(base,"GET",scoped+"/status")["volume"] == 30
                    assert api(base,"POST",scoped+"/action",{"command":"volume-up"})["accepted"]
                    assert panel(home/"plugin.sock",connection["id"],
                                 {"method":"command","function":"mute"}) == {"type":"ok"}
                    assert tv.connections == 1, "HTTP and panel must share one child/device connection"
                    assert len(api(base,"GET",scoped+"/inputs")) == 2
                    before = list(tv.commands)
                    api(base,"POST",scoped+"/action",{"command":"not-a-command"},expected=400)
                    assert tv.commands == before
                    denied = panel(home/"plugin.sock",connection["id"],{"method":"configure","settings":{}})
                    assert denied == {"type":"error","code":"unsupported"}
                    saved = (home/"connections"/connection["id"]/"plugin-connection.json")
                    assert saved.stat().st_mode & 0o777 == 0o600
                    assert json.loads(saved.read_text())["token"] == "PRIVATE-TOKEN"
                    assert "PRIVATE-TOKEN" not in json.dumps(api(base,"GET","/api/config"))
                    # Admission validates saved settings without touching the
                    # device or replacing the active selection on rejection.
                    selection = home / "integrations/state/echo"
                    old_selection = selection.read_bytes()
                    old_settings = saved.read_bytes()
                    invalid_settings = json.loads(old_settings)
                    invalid_settings["port"] = 0
                    saved.write_text(json.dumps(invalid_settings))
                    refused = subprocess.run(command, capture_output=True)
                    assert refused.returncode != 0
                    assert b"saved connection settings" in refused.stderr
                    assert selection.read_bytes() == old_selection
                    assert json.loads(saved.read_text())["port"] == 0
                    saved.write_bytes(old_settings)
                    subprocess.run(command, check=True, capture_output=True)
                    assert selection.read_bytes() == old_selection
                    assert saved.read_bytes() == old_settings
                    assert tv.connections == 1, "admission must not contact configured devices"
                    # Existing connections survive package absence, but no stale
                    # cached child may execute new commands after removal.
                    subprocess.run([str(confd),"integrations","--root",str(home/"integrations"),"remove","echo"],check=True)
                    api(base,"GET",scoped+"/status",expected=400)
                    assert api(base,"GET","/api/config")["connections"][0]["id"] == connection["id"]
                    print("Integration product flow passed: admission, catalog, private settings, HTTP/panel shared ownership, command gate, missing-package preservation.")
                finally:
                    process.terminate()
                    try: process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                    tv.shutdown()
                    if keep:
                        keep.mkdir(parents=True, exist_ok=True)
                        shutil.copy2(home/"daemon.log", keep/"integration-e2e.log")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--confd",type=Path,default=ROOT/"daemon/target/debug/couch-confd")
    parser.add_argument("--echo",type=Path,default=ROOT/"clients/target/debug/couch-plugin-echo")
    parser.add_argument("--echo-pair",type=Path,
        help="run the protocol 3 pairing leg with this package instead; both it and --confd "
             "must be built with the preview feature")
    parser.add_argument("--logs",type=Path)
    args = parser.parse_args()
    if args.echo_pair:
        run_pairing(args.confd.resolve(),args.echo_pair.resolve(),args.logs)
    else:
        run(args.confd.resolve(),args.echo.resolve(),args.logs)
