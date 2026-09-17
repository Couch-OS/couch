"""Load the installer-owned, reviewed pin helpers for release tooling.

The installer source export must carry every helper it embeds.  Keep these
small release-side adapters free of duplicated pin data and implementation.
"""
from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path
import sys


PINS = Path(__file__).resolve().parents[1] / "installer" / "pins"
_ALLOWED = frozenset(
    {
        "official_runtime",
        "prepare_official_inputs",
        "private_vendor",
        "host_dependencies",
        "mtk_dependencies",
    }
)


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
    source = PINS / (name + ".py")
    spec = spec_from_file_location(key, source)
    if spec is None or spec.loader is None:
        raise RuntimeError("installer pin helper is unavailable")
    module = module_from_spec(spec)
    sys.modules[key] = module
    spec.loader.exec_module(module)
    return module
