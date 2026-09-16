"""Experimental adapter for an already connected, externally established MTK DA session.

No session establishment or flash-writing methods are exposed. Hardware validation
is pending. See docs/installer.md before connecting this to a physical session.
"""
import hashlib
import os
from pathlib import Path
import shutil
import struct
import zlib

from couch_install import CHUNK, IDENTITY_PARTITIONS, InstallError, REPO, digest, layout, require, save_json, sync_directory

# Pinned upstream checkout, source reviewed 2026-09-09. Not a hardware certification.
REVIEWED_REVISION = "60e07f3b343a4469389f15967626d63e049968d4"
SECTOR = 512


def gpt_header(data, lba, capacity):
    require(len(data) == SECTOR and data[:8] == b"EFI PART", "Missing GPT header")
    revision, size, checksum, reserved = struct.unpack_from("<IIII", data, 8)
    require(revision == 0x10000 and 92 <= size <= SECTOR and reserved == 0, "Unsupported GPT header")
    checked = bytearray(data[:size])
    checked[16:20] = b"\0" * 4
    require(zlib.crc32(checked) == checksum, "GPT header CRC mismatch")
    current, alternate, first, last = struct.unpack_from("<QQQQ", data, 24)
    table, count, stride, table_crc = struct.unpack_from("<QIII", data, 72)
    sectors = capacity // SECTOR
    require(current == lba and alternate == (sectors - 1 if lba == 1 else 1), "GPT header locations differ")
    require(1 < first <= last < sectors - 1, "Invalid GPT usable range")
    require(0 < count <= 4096 and stride >= 128 and stride % 128 == 0
            and count * stride <= CHUNK, "Unsupported GPT entry array")
    table_end = table + (count * stride + SECTOR - 1) // SECTOR
    require((1 < table < table_end <= first) if lba == 1
            else (last < table < table_end <= lba), "GPT table overlaps usable storage")
    return {"guid": data[56:72], "first": first, "last": last, "table": table,
            "count": count, "stride": stride, "crc": table_crc}


def observed_layout(read, capacity):
    """Require both GPT copies and their entry CRCs to agree before partition reads."""
    headers, arrays = [], []
    for lba in (1, capacity // SECTOR - 1):
        header = gpt_header(read(lba * SECTOR, SECTOR), lba, capacity)
        length = header["count"] * header["stride"]
        raw = read(header["table"] * SECTOR, ((length + SECTOR - 1) // SECTOR) * SECTOR)[:length]
        require(zlib.crc32(raw) == header["crc"], "GPT entry CRC mismatch")
        headers.append(header)
        arrays.append(raw)
    require(all(headers[0][key] == headers[1][key] for key in ("guid", "first", "last", "count", "stride"))
            and arrays[0] == arrays[1], "Primary and backup GPT differ")
    header = headers[0]
    partitions = {}
    for offset in range(0, len(arrays[0]), header["stride"]):
        entry = arrays[0][offset:offset + header["stride"]]
        if entry[:16] == b"\0" * 16:
            continue
        first, last = struct.unpack_from("<QQ", entry, 32)
        require(header["first"] <= first <= last <= header["last"], "Partition outside GPT usable range")
        name = entry[56:128].decode("utf-16-le").split("\0", 1)[0]
        require(name not in partitions, "Duplicate GPT partition name")
        partitions[name] = {"offset": first * SECTOR, "size": (last - first + 1) * SECTOR}
    return layout(partitions)


class ConnectedMtkReader:
    """Read-only facade over the reviewed MTK legacy API; does not open USB itself.

    The caller owns session lifetime and must verify loaded upstream code/loader.
    A revision string records compatibility, not proof of trustworthy loaded code.
    MT6580 + a matching layout is a candidate identity, not proof of HA100 model.
    """
    def __init__(self, mtk, revision):
        require(revision == REVIEWED_REVISION, "Unreviewed mtkclient API revision")
        require(mtk.config.hwcode == 0x6580, "Expected MT6580 hardware")
        da = mtk.daloader.daconfig
        require(da.storage.flashtype == "emmc", "Only eMMC user storage is supported")
        emmc = da.legacy_storage.emmc
        capacity = emmc.m_emmc_ua_size
        require(type(capacity) is int and capacity >= 1024 * 1024 and capacity % SECTOR == 0,
                "Invalid observed eMMC capacity")
        cid = emmc.m_emmc_cid
        require(isinstance(cid, (list, tuple)) and len(cid) == 2
                and all(type(word) is int and 0 <= word < 2**64 for word in cid)
                and cid not in ([0, 0], (0, 0), [2**64-1, 2**64-1], (2**64-1, 2**64-1)),
                "Missing observed eMMC CID")
        self._readflash = mtk.daloader.readflash
        self.capacity = capacity
        wire_cid = struct.pack(">QQ", *cid)
        # Pinned Legacy_EmmcInfo parses the wire as two BE qwords; its repr
        # reverses each qword and is not canonical. The tested MT6580 DA sends
        # four LE u32 CID registers. Linux sysfs renders those registers in BE
        # order. This fixed conversion matched the same unit's runtime CID;
        # never search byte permutations to make an identity check pass.
        runtime_cid = struct.pack(">IIII", *struct.unpack("<IIII", wire_cid))
        self.runtime_cid = runtime_cid.hex()  # Private native enrollment, never public release metadata.
        self.description = {"schema": 1, "transport": "mtkclient-connected-readonly", "revision": revision,
                            "hwcode": 0x6580, "model_verified": False, "capacity": capacity,
                            # Stable within this adapter; never infer the vendor Device ID from it.
                            "storage_id": hashlib.sha256(wire_cid).hexdigest(),
                            "runtime_cid_sha256": hashlib.sha256(runtime_cid).hexdigest(),
                            "cid_encoding": "mt6580-legacy-le32-registers",
                            "partitions": observed_layout(self._read, capacity)}

    def _read(self, offset, size):
        require(type(offset) is int and type(size) is int and offset >= 0
                and 0 < size <= CHUNK and offset % SECTOR == 0 and size % SECTOR == 0
                and offset + size <= self.capacity, "Read exceeds storage or transfer bounds")
        # This revision's legacy in-memory loop computes remaining bytes
        # incorrectly after the first packet. Stay within its 1 MiB packet and
        # verify exact length. Never use its background file writer for backups.
        try:
            data = self._readflash(addr=offset, length=size, filename="", parttype="user", display=False)
        except Exception as error:
            raise InstallError(f"eMMC user read failed at offset={offset:#x}, size={size}: {error}") from error
        require(isinstance(data, (bytes, bytearray)) and len(data) == size, "Short or failed MTK read")
        return data

    def chunks(self, name):
        require(name in self.description["partitions"], "Unknown partition")
        region = self.description["partitions"][name]
        for position in range(0, region["size"], CHUNK):
            yield self._read(region["offset"] + position, min(CHUNK, region["size"] - position))

    def hash(self, name):
        result = hashlib.sha256()
        for data in self.chunks(name):
            result.update(data)
        return result.hexdigest()

    def backup_identity(self, destination, confirm, *, progress=None):
        require(confirm == self.description["storage_id"], "Storage confirmation mismatch")
        destination = Path(destination).absolute()
        require(not destination.is_symlink() and not destination.exists(), "Use a new backup destination")
        require(not destination.resolve().is_relative_to(REPO), "Keep per-device backups outside the repository")
        destination.mkdir(mode=0o700, parents=True)
        needed = sum(self.description["partitions"][name]["size"] for name in IDENTITY_PARTITIONS) + CHUNK
        require(shutil.disk_usage(destination).free >= needed, "Insufficient space for identity backup")
        report = {"schema": 1, "complete": False, "device": self.description, "backups": {},
                  "identity_decoded": False}
        save_json(destination / "readback.json", report)
        for name in sorted(IDENTITY_PARTITIONS):
            if progress:
                progress(f"Backing up identity partition: {name}")
            path = destination / f"{name}.img"
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, "wb") as output:
                for data in self.chunks(name):
                    output.write(data)
                output.flush()
                os.fsync(output.fileno())
            sync_directory(destination)
            checksum = digest(path)
            if progress:
                progress(f"Verifying independent readback: {name}")
            require(checksum == self.hash(name), f"Independent identity readback mismatch: {name}")
            report["backups"][name] = checksum
            save_json(destination / "readback.json", report)
        report["complete"] = True
        save_json(destination / "readback.json", report)
        return report
