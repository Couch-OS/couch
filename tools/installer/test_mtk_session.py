import hashlib
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace as NS
import unittest
from unittest.mock import patch
from contextlib import redirect_stderr
import io

from couch_install import InstallError
import mtk_session as session
from test_mtk_readonly import fake_session


class SessionGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.loader = self.root / "loader.bin"
        self.loader.write_bytes(b"test loader")
        self.loader_hash = hashlib.sha256(self.loader.read_bytes()).hexdigest()
        self.candidate = session.Candidate(1, 2, (3,), 0x0e8d, 0x2000)
        self.events = []
        self.mtk, _, _ = fake_session()
        self.backend = NS(
            enumerate=lambda: [self.candidate],
            claim=lambda candidate: self.events.append("claim"),
            claimed_candidate=lambda: self.candidate,
            start_readonly=self.start,
            close=lambda reset: self.events.append(("close", reset)),
        )

    def start(self, data, policy):
        self.assertEqual(data, b"test loader")
        self.assertFalse(policy.allow_flash_writes or policy.allow_reset or policy.allow_security_bypass or policy.allow_reconnect)
        self.events.append("start")
        return self.mtk

    def gate(self):
        return session.read_session(self.root, self.loader, self.loader_hash,
                                    self.root / "locks", self.candidate, lambda path: self.backend)

    def test_success_and_body_failure_close_without_reset(self):
        with patch.object(session, "source_pin", return_value={}):
            with self.assertRaisesRegex(RuntimeError, "consumer"):
                with self.gate() as reader:
                    self.assertFalse(reader.description["model_verified"])
                    raise RuntimeError("consumer")
        self.assertEqual(self.events, ["claim", "start", ("close", False)])
        with session.exclusive_lock(self.root / "locks"):
            pass  # Previous session released it on failure.

    def test_explicit_boot_only_after_successful_consumer(self):
        self.backend.boot_after_capture = lambda: self.events.append("boot")
        for fail in (False, True):
            self.events.clear()
            with patch.object(session, "source_pin", return_value={}):
                try:
                    with session.read_session(self.root, self.loader, self.loader_hash,
                            self.root / "locks", self.candidate, lambda path: self.backend,
                            boot_after_capture=True):
                        if fail:
                            raise RuntimeError("backup failed")
                except RuntimeError:
                    self.assertTrue(fail)
            self.assertEqual(self.events, ["claim", "start"] +
                             ([] if fail else ["boot"]) + [("close", False)])

    def test_import_and_loader_preparation_precedes_wait_window(self):
        self.backend.prepare = lambda data: self.events.append("prepare")
        def wait(enumerate_devices):
            self.events.append("wait")
            return self.candidate
        with patch.object(session, "source_pin", return_value={}):
            with session.read_session(self.root, self.loader, self.loader_hash, self.root / "locks",
                                      None, lambda path: self.backend, candidate_provider=wait):
                pass
        self.assertEqual(self.events, ["prepare", "wait", "claim", "start", ("close", False)])

    def test_cleanup_error_does_not_mask_startup_failure(self):
        def fail(**kwargs):
            raise OSError("cleanup failure")
        self.backend.close = fail
        with patch.object(session, "source_pin", return_value={}), redirect_stderr(io.StringIO()) as output:
            with self.assertRaisesRegex(RuntimeError, "original"):
                with self.gate():
                    raise RuntimeError("original")
        self.assertIn("cleanup failure", output.getvalue())

    def test_loader_mismatch_precedes_backend_claim(self):
        self.loader.write_bytes(b"changed")
        with patch.object(session, "source_pin", return_value={}):
            with self.assertRaisesRegex(InstallError, "approved hash"), self.gate():
                pass
        self.assertEqual(self.events, [])

    def test_multiple_candidates_prevent_claim(self):
        self.backend.enumerate = lambda: [self.candidate, session.Candidate(1, 4, (5,), 0x0e8d, 3)]
        with patch.object(session, "source_pin", return_value={}):
            with self.assertRaisesRegex(InstallError, "exactly one"), self.gate():
                pass
        self.assertEqual(self.events, [("close", False)])

    def test_recovery_gadget_is_not_a_download_candidate(self):
        runtime = session.Candidate(1, 2, (3,), 0x0e8d, 0x201c)
        with self.assertRaisesRegex(InstallError, "exactly one"):
            session.select_candidate([runtime], runtime)

    def test_changed_claim_prevents_handshake(self):
        self.backend.claimed_candidate = lambda: session.Candidate(1, 8, (3,), 0x0e8d, 0x2000)
        with patch.object(session, "source_pin", return_value={}):
            with self.assertRaisesRegex(InstallError, "different USB"), self.gate():
                pass
        self.assertEqual(self.events, ["claim", ("close", False)])

    def test_failed_claim_still_closes(self):
        def fail(candidate):
            raise OSError("disconnected")
        self.backend.claim = fail
        with patch.object(session, "source_pin", return_value={}):
            with self.assertRaisesRegex(OSError, "disconnected"), self.gate():
                pass
        self.assertEqual(self.events, [("close", False)])

    def test_exclusive_lock_blocks_second_session_and_releases(self):
        with session.exclusive_lock(self.root / "locks"):
            with self.assertRaisesRegex(InstallError, "holds the USB lock"):
                with session.exclusive_lock(self.root / "locks"):
                    pass
        with session.exclusive_lock(self.root / "locks"):
            pass

    def test_loaded_module_from_other_checkout_is_rejected(self):
        with patch.dict(sys.modules, {"mtkclient.foreign": NS(__file__=str(self.root / "foreign.py"))}):
            with self.assertRaisesRegex(InstallError, "outside the source pin"):
                session.verify_loaded_sources({})

    def test_missing_source_package_is_rejected_before_usb(self):
        with self.assertRaises(InstallError):
            session.source_pin(self.root / "missing-checkout")

    def test_packaged_source_hash_inventory_and_extra_python_are_checked_without_git(self):
        import json
        repo = self.root / "source"; package = repo / "mtkclient"; package.mkdir(parents=True)
        source = package / "__init__.py"; source.write_text("# fixture\n")
        inventory = self.root / "inventory.json"
        inventory.write_text(json.dumps({"schema": 1, "kind": "couch-reviewed-mtk-source", "revision": session.REVIEWED_REVISION,
            "files": {"mtkclient/__init__.py": {"size": source.stat().st_size, "sha256": hashlib.sha256(source.read_bytes()).hexdigest()}}}))
        checksum = hashlib.sha256(inventory.read_bytes()).hexdigest()
        with patch.object(session, "SOURCE_INVENTORY", inventory), patch.object(session, "SOURCE_INVENTORY_SHA256", checksum), patch.object(subprocess, "check_output", side_effect=AssertionError("Git must not run")):
            self.assertIn(source.resolve(), session.source_pin(repo))
            source.write_text("# changed\n")
            with self.assertRaisesRegex(InstallError, "Pinned source differs"):
                session.source_pin(repo)
            source.write_text("# fixture\n"); (package / "extra.py").write_text("# extra\n")
            with self.assertRaisesRegex(InstallError, "Untracked or missing"):
                session.source_pin(repo)
            (package / "extra.py").unlink(); inventory.write_text('{}')
            with self.assertRaisesRegex(InstallError, "inventory differs"):
                session.source_pin(repo)


if __name__ == "__main__":
    unittest.main()
