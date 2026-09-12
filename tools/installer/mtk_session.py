"""Source/loader/selection gate for an experimental read-only MTK backend.

The CLI does not enable physical operations. A backend must claim an exact
candidate before starting its DA handshake; tests never open USB.
"""
from contextlib import contextmanager
from dataclasses import dataclass
import hashlib
import json
import importlib.util
import os
from pathlib import Path
import stat
import sys

from couch_install import InstallError, REPO, regular, require
from mtk_readonly import ConnectedMtkReader, REVIEWED_REVISION

DOWNLOAD_PIDS = frozenset((0x0003, 0x6000, 0x2000, 0x2001, 0x20ff, 0x3000))


@dataclass(frozen=True)
class Candidate:
    bus: int
    address: int
    ports: tuple
    vid: int
    pid: int


@dataclass(frozen=True)
class ReadPolicy:
    # Uploading a DA to RAM is active device interaction, despite read-only flash.
    allow_ram_loader: bool = True
    allow_flash_writes: bool = False
    allow_reset: bool = False
    allow_security_bypass: bool = False
    allow_reconnect: bool = False
    skip_watchdog_changes: bool = True


SOURCE_INVENTORY = Path(__file__).with_name("mtk_source_inventory.json")
SOURCE_INVENTORY_SHA256 = "d678c872b024f53c4ea69e6bd7ec9ae5e02dee297d8d33d36d360704a29a699a"


def source_pin(checkout):
    """Admit exactly the reviewed packaged source without requiring runtime Git."""
    checkout = Path(checkout).resolve()
    try:
        inventory_bytes = regular(SOURCE_INVENTORY).read_bytes()
        require(hashlib.sha256(inventory_bytes).hexdigest() == SOURCE_INVENTORY_SHA256,
                "Reviewed mtkclient source inventory differs")
        inventory = json.loads(inventory_bytes)
        require(inventory.get("schema") == 1 and inventory.get("kind") == "couch-reviewed-mtk-source"
                and inventory.get("revision") == REVIEWED_REVISION,
                "Unreviewed mtkclient source inventory")
        tracked = {}
        for name, expected in inventory["files"].items():
            if not name.startswith("mtkclient/") or not name.endswith(".py"):
                continue
            require(not Path(name).is_absolute() and ".." not in Path(name).parts,
                    "Invalid reviewed mtkclient source path")
            path = regular(checkout / name)
            require(path.resolve().is_relative_to(checkout), "Pinned source escapes checkout")
            data = path.read_bytes()
            require(len(data) == expected["size"] and hashlib.sha256(data).hexdigest() == expected["sha256"],
                    f"Pinned source differs: {name}")
            tracked[path.resolve()] = expected["sha256"]
        actual = {path.resolve() for path in (checkout / "mtkclient").rglob("*.py")}
        require(tracked and actual == set(tracked), "Untracked or missing mtkclient Python source")
        return tracked
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as error:
        raise InstallError("Cannot inspect pinned mtkclient source package; verify its receipt and files. "
                           "No USB was claimed.") from error


def verify_loaded_sources(pinned):
    for name, module in tuple(sys.modules.items()):
        if name != "mtkclient" and not name.startswith("mtkclient."):
            continue
        filename = getattr(module, "__file__", None)
        require(filename, "Unverifiable loaded mtkclient module")
        if filename.endswith(".pyc"):
            filename = importlib.util.source_from_cache(filename)
        path = Path(filename).resolve()
        require(path in pinned and hashlib.sha256(regular(path).read_bytes()).hexdigest() == pinned[path],
                f"Loaded mtkclient module is outside the source pin: {name}")


def loader_bytes(path, expected_sha256):
    require(isinstance(expected_sha256, str) and len(expected_sha256) == 64
            and all(c in "0123456789abcdef" for c in expected_sha256), "Invalid approved loader hash")
    path = regular(path)
    require(0 < path.stat().st_size <= 64 * 1024 * 1024, "Invalid loader size")
    data = path.read_bytes()
    require(hashlib.sha256(data).hexdigest() == expected_sha256, "Loader differs from approved hash")
    return data


@contextmanager
def exclusive_lock(directory):
    try:
        import fcntl
    except ImportError as error:
        raise InstallError("Installer locking currently requires Linux or macOS") from error
    directory = Path(directory).absolute()
    require(not directory.is_symlink() and not directory.resolve().is_relative_to(REPO), "Use a private lock directory outside the repository")
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = directory.stat()
    require(info.st_uid == os.getuid() and info.st_mode & 0o077 == 0, "Lock directory must be owned and private")
    fd = os.open(directory / "usb-session.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_uid == os.getuid()
                and info.st_mode & 0o077 == 0, "Invalid installer lock file")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise InstallError("Another installer session holds the USB lock") from error
        yield
    finally:
        # Do not unlink: a waiter may already hold the inode. Closing releases
        # flock even on failure; kernel process cleanup handles a crash.
        os.close(fd)


def select_candidate(observed, expected):
    matches = [item for item in observed if item.vid == 0x0e8d and item.pid in DOWNLOAD_PIDS]
    require(len(matches) == 1, "Expected exactly one MediaTek download candidate")
    require(matches[0] == expected and expected.bus is not None and expected.address is not None,
            "USB candidate changed or lacks a stable session address")
    return matches[0]


@contextmanager
def connected_session(checkout, loader, loader_sha256, lock_directory, expected, backend_factory, *, candidate_provider=None, boot_after_capture=False, device_factory=ConnectedMtkReader):
    """Gate an explicitly supplied backend and device facade for a private session.

    Startup remains read-only. The default facade exposes reads only; a private
    trial may supply a writer factory after independently validating the target.
    Public read_session does not accept this factory override.

    Factory must be side-effect-free until claim(), and expose enumerate(),
    claim(Candidate), claimed_candidate(), start_readonly(bytes, ReadPolicy),
    close(reset=False). start_readonly returns the connected upstream Mtk object.
    An explicit boot_after_capture requests the legacy normal-boot exit only
    after the consumer returns successfully; failed captures never request boot.
    The backend must not rediscover a different target or invoke configure_da's
    generic reset/security fallback. mtk_usb.ExactUsbBackend implements this
    contract experimentally. Physical DA startup and identity readback have
    passed; clean USB teardown and installer recovery remain separate milestones.
    """
    with exclusive_lock(lock_directory):
        pinned = source_pin(checkout)
        verify_loaded_sources(pinned)
        data = loader_bytes(loader, loader_sha256)
        backend = backend_factory(Path(checkout).resolve())
        try:
            if hasattr(backend, "prepare"):
                backend.prepare(data)
            # Check module provenance again after the factory's imports, before
            # claiming any USB interface or invoking any protocol operation.
            verify_loaded_sources(pinned)
            if candidate_provider is not None:
                require(expected is None, "Choose a fixed candidate or a candidate provider")
                expected = candidate_provider(backend.enumerate)
            candidate = select_candidate(backend.enumerate(), expected)
            backend.claim(candidate)
            require(backend.claimed_candidate() == candidate, "Backend claimed a different USB device")
            mtk = backend.start_readonly(data, ReadPolicy())
            verify_loaded_sources(pinned)
            yield device_factory(mtk, REVIEWED_REVISION)
            if boot_after_capture:
                backend.boot_after_capture()
        finally:
            original_error = sys.exc_info()[1]
            try:
                backend.close(reset=False)
            except Exception as cleanup_error:
                if original_error is None:
                    raise
                print(f"USB cleanup also failed: {cleanup_error}", file=sys.stderr)


def read_session(checkout, loader, loader_sha256, lock_directory, expected, backend_factory, *, candidate_provider=None, boot_after_capture=False):
    """Read-only capture entry point; never accepts a writable device factory."""
    return connected_session(checkout, loader, loader_sha256, lock_directory, expected,
                             backend_factory, candidate_provider=candidate_provider,
                             boot_after_capture=boot_after_capture)
