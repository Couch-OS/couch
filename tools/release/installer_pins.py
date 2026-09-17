"""Locate the installer submodule and load its reviewed pin helpers.

Couch consumes Couch-OS/couch-installer as the ``couch-installer`` Git
submodule. Release tooling reads installer pins, the neutral RAM image builder
and RAM stage sources from that pinned checkout instead of keeping copies.
"""
from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path, PurePosixPath
import sys


REPO = Path(__file__).resolve().parents[2]
SUBMODULE = "couch-installer"
RELATIVE = PurePosixPath(SUBMODULE, "tools", "installer")
INSTALLER = REPO / RELATIVE
PINS = INSTALLER / "pins"
_ALLOWED = frozenset(
    {
        "official_runtime",
        "prepare_official_inputs",
        "private_vendor",
        "host_dependencies",
        "mtk_dependencies",
    }
)


def installer_path(*parts, root=REPO):
    """Return a path inside the installer checkout, refusing an uninitialized submodule."""
    base = Path(root) / RELATIVE
    if not base.is_dir():
        raise RuntimeError(
            f"Installer source is missing; run: git submodule update --init {SUBMODULE}"
        )
    return base.joinpath(*parts)


def _load_file(key, source):
    spec = spec_from_file_location(key, source)
    if spec is None or spec.loader is None:
        raise RuntimeError("installer helper is unavailable: " + source.name)
    module = module_from_spec(spec)
    sys.modules[key] = module
    spec.loader.exec_module(module)
    return module


def load(name):
    """Return one canonical installer helper, loading its dependencies first."""
    if name not in _ALLOWED:
        raise ValueError("unknown installer pin helper")
    key = "_couch_installer_" + name
    if key in sys.modules:
        return sys.modules[key]
    for dependency in {
        "official_runtime": ("private_vendor",),
        "prepare_official_inputs": ("private_vendor", "official_runtime"),
    }.get(name, ()):
        sys.modules[dependency] = load(dependency)
    return _load_file(key, installer_path("pins", name + ".py"))


def load_neutral_ramdisk():
    """Return the installer-owned neutral RAM image builder."""
    key = "_couch_installer_neutral_ramdisk"
    if key in sys.modules:
        return sys.modules[key]
    return _load_file(key, installer_path("image", "neutral_ramdisk.py"))
