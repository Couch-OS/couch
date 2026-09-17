#!/usr/bin/env python3
"""Run the exact published Denon v1 APK against a frozen ARM core in a container.

Uses real apk signature/admission, local-only simulated AVR, HTTP and panel IPC.
Never contacts hardware. Inputs are public binaries/manifests/key/provenance.
The receipt is emitted only after every assertion passes; no production keys.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import socket
import socketserver
import struct
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

CORE = "f274804c5219db00446f1b9e5ce46391adcfd37c"
EXPECTED = {
    "apk": "61e135854816bd4f996e46d918386937913860e583ccdb3bda7eac739252b270",
    "key": "80f3a73d86759cda103cb4f9a876cd4caee9d25c235c6d782b4be8a900b2696c",
    "provenance": "528e0142a024575f34980d1f7c899de99f5493e43d2d1c4278d7a17db0a8a2a0",
    "manifest": "3abd01c3863b1e540d2d1317614c844071e3fd0900e1dcc403ba16680d4df540",
    "plugin": "f9d5bc52c7e0a8a77d0f456aaf876a47be5586c0e996e172119ac1efd084a24d",
}


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def command(args, success=True):
    result = subprocess.run([str(x) for x in args], capture_output=True, timeout=90)
    assert (result.returncode == 0) == success, (args[1:], result.returncode, result.stdout, result.stderr)
    return result.stdout.decode()


def api(base, method, path, value=None, status=200):
    request = urllib.request.Request(base + path, method=method,
        data=None if value is None else json.dumps(value).encode(),
        headers={"Content-Type": "application/json"})
    try:
        response = urllib.request.urlopen(request, timeout=20)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        data = response.read()
        assert response.status == status, (method, path, response.status, data)
        return json.loads(data)


def panel(path, connection, request):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(20)
        stream.connect(str(path))
        payload = json.dumps({"connection_id": connection, "request": request}).encode()
        stream.sendall(struct.pack(">I", len(payload)) + payload)
        def read(length):
            data = b""
            while len(data) < length:
                chunk = stream.recv(length - len(data))
                assert chunk, "truncated local protocol reply"
                data += chunk
            return data
        length = struct.unpack(">I", read(4))[0]
        assert 0 < length <= 65536
        return json.loads(read(length))


class AVR(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True
    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.lock = threading.Lock()
        self.connections = 0
        self.active = 0
        self.maximum_active = 0
        self.requests = []
        self.muted = False
        self.volume = "455"


class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        avr = self.server
        with avr.lock:
            avr.connections += 1
            avr.active += 1
            avr.maximum_active = max(avr.maximum_active, avr.active)
        data = b""
        try:
            while chunk := self.request.recv(1024):
                data += chunk
                while b"\r" in data:
                    raw, data = data.split(b"\r", 1)
                    request = raw.decode("ascii")
                    with avr.lock:
                        avr.requests.append(request)
                        if request == "ZM?": reply = "ZMON"
                        elif request == "MV?": reply = "MV" + avr.volume
                        elif request == "MU?": reply = "MUON" if avr.muted else "MUOFF"
                        elif request == "SI?": reply = "SIBD"
                        elif request == "SSFUN ?": reply = "SSFUNBD Blu-ray\rSSFUNSAT/CBL Cable\rSSFUN END"
                        elif request == "MVUP":
                            avr.volume = "46"
                            reply = "MV46"
                        elif request == "MUON":
                            avr.muted = True
                            reply = "MUON"
                        else: raise AssertionError("unexpected AVR wire command: " + request)
                    self.request.sendall((reply + "\r").encode("ascii"))
        finally:
            with avr.lock: avr.active -= 1


def run(args):
    assert args.core_commit == CORE
    for key in ("apk", "key", "provenance"):
        assert digest(getattr(args, key)) == EXPECTED[key], "wrong public " + key
    core_bytes = args.confd.read_bytes()
    assert core_bytes[:5] == b"\x7fELF\x01" and struct.unpack("<H", core_bytes[18:20])[0] == 40, "core must be ARM32 ELF"
    command([args.confd, "--supports-integration-protocol=1"])
    command([args.confd, "--supports-integration-protocol=2"])
    checks = {}
    with tempfile.TemporaryDirectory(prefix="couch-v1-compat-", dir="/tmp") as temporary:
        home = Path(temporary)
        home.chmod(0o755)  # subprocess is deliberately dropped to nobody
        keys = home / "keys"
        keys.mkdir()
        shutil.copyfile(args.key, keys / args.key.name)
        command(["apk", "--keys-dir", keys, "verify", args.apk])
        store = home / "integrations"
        install = [args.confd, "integrations", "--root", store, "--keys-dir", keys, "install-sideload", args.apk]
        command(install)
        assert command([args.confd, "integrations", "--root", store, "list"]).strip() == "denon 0.1.1"
        manifests = list((store / "slots" / "denon").glob("*/manifest.json"))
        assert len(manifests) == 1
        manifest_path = manifests[0]
        assert digest(manifest_path) == EXPECTED["manifest"]
        manifest = json.loads(manifest_path.read_text())
        assert manifest["protocol_version"] == 1 and manifest["version"] == "0.1.1"
        plugin = manifest_path.parent / manifest["executable"]
        assert digest(plugin) == EXPECTED["plugin"]
        plugin_bytes = plugin.read_bytes()
        assert plugin_bytes[:5] == b"\x7fELF\x01" and struct.unpack("<H", plugin_bytes[18:20])[0] == 40
        empty = home / "untrusted-keys"
        empty.mkdir()
        command([args.confd,"integrations","--root",home/"untrusted-store","--keys-dir",empty,"install-sideload",args.apk], success=False)
        assert not (home/"untrusted-store/state/denon").exists()
        tampered = home / "tampered.apk"
        tampered.write_bytes(args.apk.read_bytes() + b"tampered")
        command([args.confd,"integrations","--root",home/"tampered-store","--keys-dir",keys,"install-sideload",tampered], success=False)
        assert not (home/"tampered-store/state/denon").exists()
        with AVR() as avr:
            threading.Thread(target=avr.serve_forever, daemon=True).start()
            with socket.socket() as reservation:
                reservation.bind(("127.0.0.1", 0))
                port = reservation.getsockname()[1]
            base = f"http://127.0.0.1:{port}"
            with (home/"daemon.log").open("w+") as log:
                process = subprocess.Popen([str(args.confd),"--addr",f"127.0.0.1:{port}","--config",str(home/"config.json"),"--pin-file",str(home/"pin"),"--no-auth"],stdout=log,stderr=log)
                try:
                    for _ in range(200):
                        try:
                            api(base,"GET","/api/health")
                            break
                        except (OSError,AssertionError):
                            if process.poll() is not None:
                                log.seek(0)
                                raise AssertionError(log.read())
                            time.sleep(0.05)
                    else: raise AssertionError("daemon startup deadline")
                    live = api(base,"GET","/api/integrations")["integrations"]
                    assert len(live) == 1 and live[0]["protocol_version"] == 1
                    config = api(base,"POST","/api/connections",{"name":"Simulated receiver","provider":{"kind":"plugin","id":"denon","label":"untrusted label","capabilities":[]}})
                    connection = config["connections"][0]["id"]
                    scoped = f"/api/connections/{connection}/plugin"
                    configured = api(base,"POST",scoped+"/settings",{"host":"127.0.0.1","port":avr.server_address[1]})
                    assert configured["configured"] and avr.connections == 0
                    first = api(base,"GET",scoped+"/status")
                    assert first == {"on":True,"muted":False,"input":"BD"}, first
                    checks["v1_handshake"] = "passed"
                    checks["fake_receiver_status"] = "passed"
                    inputs = api(base,"GET",scoped+"/inputs")
                    assert inputs == [{"id":"BD","name":"Blu-ray"},{"id":"SAT/CBL","name":"Cable"}], inputs
                    checks["fake_receiver_inputs"] = "passed"
                    assert api(base,"POST",scoped+"/action",{"command":"volume-up"})["accepted"]
                    assert panel(home/"plugin.sock",connection,{"method":"command","function":"mute-on"}) == {"type":"ok"}
                    assert api(base,"GET",scoped+"/status") == {"on":True,"muted":True,"input":"BD"}
                    expected = ["ZM?","MV?","MU?","SI?","SSFUN ?","MVUP","MV?","MUON","MU?","ZM?","MV?","MU?","SI?"]
                    assert avr.requests == expected, avr.requests
                    assert avr.connections == 1 and avr.maximum_active == 1
                    checks["fake_receiver_command"] = "passed"
                    checks["shared_transport_ownership"] = "passed"
                    api(base,"POST",scoped+"/typed-action",{"action":"set_volume_db","tenths":-345},status=400)
                    assert panel(home/"plugin.sock",connection,{"method":"action","action":{"action":"set_volume_db","tenths":-345}}) == {"type":"error","code":"unsupported"}
                    assert avr.requests == expected and avr.connections == 1
                    checks["v2_action_refused_for_v1"] = "passed"
                    settings_path = home/"connections"/connection/"plugin-connection.json"
                    settings_before = settings_path.read_bytes()
                    assert settings_path.stat().st_mode & 0o777 == 0o600
                    selection_before = (store/"state/denon").read_bytes()
                    command(install)
                    assert settings_path.read_bytes() == settings_before
                    assert (store/"state/denon").read_bytes() == selection_before
                    assert avr.requests == expected and avr.connections == 1
                    command([args.confd,"integrations","--root",store,"remove","denon"])
                    api(base,"GET",scoped+"/status",status=400)
                    assert api(base,"GET","/api/config")["connections"][0]["id"] == connection
                    assert settings_path.read_bytes() == settings_before and avr.requests == expected
                    # Stop before reinstall: lifecycle preservation does not
                    # depend on reuse of a daemon's retired owner after removal.
                    process.terminate()
                    process.wait(timeout=10)
                    command(install)
                    assert command([args.confd,"integrations","--root",store,"list"]).strip() == "denon 0.1.1"
                    assert settings_path.read_bytes() == settings_before
                    assert avr.requests == expected
                    checks["signed_package_lifecycle"] = "passed"
                    report = {"schema":1,"kind":"couch-integration-host-test-report","core_commit":CORE,
                        "checks":checks,"signature_checks":{"trusted_apk":"passed","untrusted_key":"rejected","tampered_apk":"rejected"},
                        "lifecycle":["signed_install","same_version_readmission","removal_preserves_config_and_settings","signed_reinstall_preserves_settings"],
                        "wire_requests":expected,"http_panel_device_connections":1,"maximum_simultaneous_device_connections":1,
                        "hardware_validation":False}
                finally:
                    if process.poll() is None:
                        process.terminate()
                        try: process.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()
                    avr.shutdown()
    args.out.mkdir(parents=True,exist_ok=True)
    report_path = args.out / "denon-0.1.1-core-f274804-host-report.json"
    report_path.write_text(json.dumps(report,indent=2,sort_keys=True)+"\n")
    receipt = {"schema":1,"kind":"couch-integration-host-compatibility","evidence_level":"host-protocol-compatibility",
        "core":{"source_commit":CORE,"supported_protocol_versions":[1,2],"target":"armv7-unknown-linux-musleabihf","binary_sha256":digest(args.confd)},
        "integrations":[{"id":"denon","version":"0.1.1","protocol_version":1,"apk_sha256":EXPECTED["apk"],"manifest_sha256":EXPECTED["manifest"],"binary_sha256":EXPECTED["plugin"],"provenance_sha256":EXPECTED["provenance"]}],
        "checks":checks,"hardware_validation":False,"harness_sha256":digest(__file__),"report_sha256":digest(report_path)}
    receipt_path = args.out / "denon-0.1.1-core-f274804-host-compatibility.json"
    receipt_path.write_text(json.dumps(receipt,indent=2,sort_keys=True)+"\n")
    print(json.dumps(receipt,indent=2,sort_keys=True))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["confd","apk","key","provenance","out"]:
        parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--core-commit",required=True)
    run(parser.parse_args())
