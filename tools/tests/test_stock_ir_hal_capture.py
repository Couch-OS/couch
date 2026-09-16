"""Private-fixture offline tests; run with COUCH_STOCK_IR_HAL set."""
import importlib.util
import os
from pathlib import Path
import unittest

HAL = os.environ.get('COUCH_STOCK_IR_HAL')


@unittest.skipUnless(HAL, 'Set COUCH_STOCK_IR_HAL to the private stock ARM HAL')
class StockHalCaptureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        path = Path(__file__).resolve().parents[1] / 'capture-stock-ir-hal.py'
        spec = importlib.util.spec_from_file_location('capture_hal', path)
        cls.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.module)

    def capture(self, mode):
        capture = self.module.Capture(HAL, {'irtx.hal.mode': mode})
        result = capture.transmit(38000, [9000, 4500, 560, 560, 560, 1690, 560])
        self.assertEqual(result['open_result'], 0)
        self.assertEqual(result['transmit_result'], 0)
        self.assertEqual(len(capture.writes), 1)
        self.assertEqual([e['ioctl'] for e in result['events'] if 'ioctl' in e], ['0x80045201'])
        return capture.writes[0]

    def test_stock_carrier_is_nine_high_seventeen_low_microsecond_samples(self):
        data = self.capture('0')
        bits = [(byte >> bit) & 1 for byte in data[:13] for bit in range(8)]
        self.assertEqual(bits, ([1]*9+[0]*17)*4)

    def test_execution_is_deterministic_and_mode_changes_waveform(self):
        mode_zero = self.capture('0')
        self.assertEqual(mode_zero, self.capture('0'))
        self.assertNotEqual(mode_zero, self.capture('1'))

    def test_guest_syscalls_are_denied(self):
        capture = self.module.Capture(HAL, {})
        with self.assertRaisesRegex(RuntimeError, 'denied'):
            capture.interrupt(capture.uc, 2, None)


if __name__ == '__main__':
    unittest.main()
