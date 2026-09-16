from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('ha100-power-snapshot.sh')


class PowerSnapshotTests(unittest.TestCase):
    def snapshot(self, files):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, value in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                if value is None:
                    path.mkdir()
                else:
                    path.write_text(value)
            before = {name: (root / name).read_bytes() for name, value in files.items() if value is not None}
            result = subprocess.check_output(['sh', str(SCRIPT), directory], text=True, timeout=3)
            self.assertEqual(before, {name: (root / name).read_bytes() for name in before})
            return result

    def test_only_allowlisted_fields_and_pins_are_reported(self):
        result = self.snapshot({'sys/class/power_supply/battery/capacity': '100\n',
            'sys/class/power_supply/battery/serial_number': 'PRIVATE',
            'sys/class/misc/mtgpio/pin': '  4:00111110\n 17:00111110\n 58:00001110\n',
            'tmp/couch-gui.health': '12 100\n', 'proc/12/comm': 'couch-gui\n',
            'proc/uptime': '102.7 44.8\n'})
        self.assertNotIn('PRIVATE', result)
        self.assertNotIn('gpio.4.', result)
        self.assertIn('gpio.17.mode=0 din=1 dout=1 direction=1', result)
        self.assertIn('gui.health_age_seconds=2', result)
        self.assertIn('gui.health_fresh=yes', result)

    def test_cached_gauge_allows_only_known_fields_and_units(self):
        result = self.snapshot({'sys/class/power_supply/battery/couch_gauge':
            'schema=1\nready=1\nsequence=13\nmethod=software\ncalibration=unverified\n'
            'current_estimate_ua=-497100\ncurrent_estimate_positive=discharge\n'
            'temperature_fixed=1\nprofile_qmax_25_mah=2535\n'
            'serial_number=PRIVATE\nmethod=PRIVATE\nvoltage_mv=PRIVATE\n',
            'proc/hps/num_base_perf_serv': '1\n',
            'sys/devices/system/cpu/online': '0-1\n'})
        self.assertIn('power.gauge.method=software\n', result)
        self.assertIn('power.gauge.current_estimate_ua=-497100\n', result)
        self.assertIn('power.gauge.current_estimate_positive=discharge\n', result)
        self.assertIn('power.gauge.temperature_fixed=1\n', result)
        self.assertIn('cpu.active_floor=1\n', result)
        self.assertIn('cpu.online=0-1\n', result)
        self.assertNotIn('PRIVATE', result)

    def test_failed_reads_and_legacy_kernels_do_not_abort_snapshot(self):
        # A directory is readable but fails read(2), like a sysfs ENODATA node.
        result = self.snapshot({'sys/class/power_supply/battery/batt_temp': None,
            'sys/class/power_supply/battery/couch_gauge': None,
            'sys/class/power_supply/battery/BatterySenseVoltage': '4200\n'})
        self.assertIn('power.battery.batt_temp=unavailable\n', result)
        self.assertIn('power.battery.BatterySenseVoltage=4200\n', result)
        self.assertIn('power.gauge=unavailable\n', result)
        self.assertIn('gui.health=unavailable\n', result)
        self.assertIn('power.gauge=unavailable\n', self.snapshot({}))

    def test_absent_stale_and_malformed_health(self):
        self.assertIn('gui.health=unavailable', self.snapshot({}))
        for stamp in ('99', '999'):
            result = self.snapshot({'tmp/couch-gui.health': f'12 {stamp}\n',
                'proc/12/comm': 'couch-gui\n', 'proc/uptime': '200.5 0\n'})
            self.assertIn('gui.health_fresh=no', result)
        self.assertIn('gui.health=invalid', self.snapshot({
            'tmp/couch-gui.health': '../escape bad\n', 'proc/uptime': '100.5 0\n'}))


if __name__ == '__main__':
    unittest.main()
