"""Boot policy regressions: outages must not expose an automatic hotspot."""
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / "stage2/setup-mode.sh"
STAGE2 = SCRIPT.with_name("stage2.sh")
HARDWARE = SCRIPT.with_name("hardware-init.sh")


class SetupModeTests(unittest.TestCase):
    def mode(self, networks=0, no_ui=0, hotspot=0):
        return subprocess.check_output(
            ["sh", str(SCRIPT), str(networks), str(no_ui), str(hotspot)], text=True
        ).strip()

    def test_fresh_device_uses_local_wifi(self):
        self.assertEqual(self.mode(), "local")

    def test_saved_networks_stay_normal_even_without_connectivity(self):
        for count in [1, 2, 20]:
            self.assertEqual(self.mode(count), "normal")

    def test_headless_recovery_does_not_wait_for_an_unavailable_gui(self):
        self.assertEqual(self.mode(no_ui=1), "normal")

    def test_hotspot_requires_an_explicit_request(self):
        for count in [0, 1]:
            for no_ui in [0, 1]:
                self.assertEqual(self.mode(count, no_ui, 1), "recovery")


class NetworkSettleTests(unittest.TestCase):
    """The setup decision runs behind the GUI now, so it also has to hand the
    GUI the answer: /tmp/couch.onboarding, then the pending marker removed."""

    def body(self):
        text = STAGE2.read_text()
        start = text.index("network_settle() {")
        return text[start:text.index("\n}\n", start) + 3]

    def settle(self, saved=0, nets=0, ip=""):
        with tempfile.TemporaryDirectory(prefix="couch-settle-") as temporary:
            root = Path(temporary)
            (root / "opt/couch").mkdir(parents=True)
            (root / "opt/couch/networks.conf").write_text("network={\n" * saved)
            (root / "couch.network-pending").write_text("")
            system = root / "couch-system"
            system.write_text('#!/bin/sh\necho "$*" >> %s/calls\n' % root)
            system.chmod(0o755)
            script = root / "settle.sh"
            script.write_text(
                "BB=\nSYSTEM=%s\nNETS=%d\nIP=%s\n" % (system, nets, ip)
                + self.body()
                .replace("/mnt/alpine/opt/couch", str(root / "opt/couch"))
                .replace('$(dirname "$0")', str(SCRIPT.parent))
                .replace("/tmp/", str(root) + "/")
                + "\nnetwork_settle\n"
            )
            subprocess.run(["sh", str(script)], check=True, capture_output=True, timeout=10)
            calls = root / "calls"
            return (
                (root / "couch.onboarding").exists(),
                (root / "couch.network-pending").exists(),
                calls.read_text().split() if calls.exists() else [],
            )

    def test_a_fresh_remote_is_told_to_onboard_and_the_marker_is_dropped(self):
        onboarding, pending, calls = self.settle()
        self.assertTrue(onboarding)
        self.assertFalse(pending)
        self.assertEqual(calls, [])

    def test_saved_networks_never_produce_the_onboarding_file(self):
        for saved, nets in [(1, 0), (0, 3), (2, 5)]:
            onboarding, pending, _ = self.settle(saved=saved, nets=nets)
            self.assertFalse(onboarding, (saved, nets))
            self.assertFalse(pending, (saved, nets))

    def test_ssh_starts_only_once_the_lease_landed(self):
        self.assertEqual(self.settle(saved=1, ip="10.0.0.5")[2], ["ssh-start"])
        self.assertEqual(self.settle(saved=1)[2], [])

    def test_the_marker_outlives_the_onboarding_file_it_answers_for(self):
        # A GUI that saw the marker gone before the file appeared would decide
        # "no onboarding" and show the room UI to a remote nobody has set up.
        body = self.body()
        self.assertLess(
            body.index("/tmp/couch.onboarding"),
            body.index("rm -f /tmp/couch.network-pending"),
        )

    def test_dev_nodes_are_populated_before_the_radio_block(self):
        # The GUI opens /dev/input/eventN, and with no devtmpfs those nodes
        # exist only after an mdev sweep. It starts beside the radio now, so
        # the sweep cannot live inside radio_up().
        text = HARDWARE.read_text()
        self.assertLess(text.index("\n$BB mdev -s\n"), text.index("\nradio_up() {\n"))


if __name__ == "__main__":
    unittest.main()
