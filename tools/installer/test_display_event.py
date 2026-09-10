import unittest
from display_event import Event, from_host_counter


class EventTests(unittest.TestCase):
    def test_same_host_counters_and_unknown_initial_rate(self):
        line = 'Write: userdata 1048576/4194304 bytes (25%)'
        self.assertEqual(from_host_counter(line).encode(), 'v1 write userdata 1048576 4194304 0 waiting none\n')
        self.assertEqual(from_host_counter(line, elapsed=2, wifi='connected').rate, 524288)

    def test_secrets_control_characters_and_unknown_fields_rejected(self):
        for line in ('SSID=secret', '\x1b[2JWrite: boot 1/2 bytes (50%)', 'Write: secret 1/2 bytes (50%)'):
            with self.assertRaises(ValueError): from_host_counter(line)
        for event in (Event(phase='password'), Event(target='../nvram'), Event(wifi='my SSID'), Event(error='secret')):
            with self.assertRaises(ValueError): event.encode()

    def test_invalid_counters_and_replay_sampling_rejected(self):
        for event in (Event(done=2,total=1), Event(done=-1), Event(rate=True), Event(total=2**64)):
            with self.assertRaises(ValueError): event.encode()
        for line in ('Write: boot 1/0 bytes (0%)', 'Write: boot 1/2 bytes (75%)'):
            with self.assertRaises(ValueError): from_host_counter(line)
        with self.assertRaises(ValueError): from_host_counter('Write: boot 1/2 bytes (50%)', elapsed=float('nan'))


if __name__ == '__main__':
    unittest.main()
