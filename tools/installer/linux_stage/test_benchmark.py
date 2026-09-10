import unittest
from benchmark import header, read_exact, response

class Tests(unittest.TestCase):
    def test_bounds(self):
        self.assertEqual(len(header(1, 64*1048576)), 16)
        for op, size in [(1,0), (2,64*1048576+1), (3,2), (4,0)]:
            with self.assertRaises(ValueError): header(op,size)

    def test_short_usb_packets_assemble_and_bad_response_rejected(self):
        class Endpoint:
            def __init__(self, blocks): self.blocks=iter(blocks)
            def read(self, count, **kwargs): return next(self.blocks)
        self.assertEqual(read_exact(Endpoint([b'ab',b'cd']),4),b'abcd')
        with self.assertRaises(ValueError): response(Endpoint([header(0,0)]),0)
        with self.assertRaises(ValueError): read_exact(Endpoint([b'']),1)

if __name__ == '__main__': unittest.main()
