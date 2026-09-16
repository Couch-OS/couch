#!/usr/bin/env python3
"""Host product acceptance using real confd/plugin processes and a fake TV.

Only APK extraction is mocked on the host; native signature/repository checks
are exercised separately by tools/integrations. Never contacts a real device.
Build couch-confd and couch-plugin-echo in their respective workspaces first.
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
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--confd",type=Path,default=ROOT/"daemon/target/debug/couch-confd")
    parser.add_argument("--echo",type=Path,default=ROOT/"clients/target/debug/couch-plugin-echo")
    parser.add_argument("--logs",type=Path)
    args = parser.parse_args()
    run(args.confd.resolve(),args.echo.resolve(),args.logs)
