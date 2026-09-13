import json
import struct
from types import SimpleNamespace as NS
import unittest
from couch_install import InstallError
from mtk_session import Candidate
from stage_usb import StageUsb


class Interface(list):
    bInterfaceClass=0xff
    bInterfaceSubClass=0
    bInterfaceProtocol=0
    bNumEndpoints=2
    bInterfaceNumber=3


class StageTests(unittest.TestCase):
    def fixture(self, platform='linux'):
        incoming=bytearray(struct.pack('<4sIQ',b'CBR1',0,4)+b'CBP1')
        writes=[]
        claims=[]
        def read(size,**kwargs):
            value=bytes(incoming[:size]);del incoming[:size];return value
        outgoing=NS(bmAttributes=2,bEndpointAddress=2,write=lambda data,**kw:writes.append(data) or len(data))
        endpoint=NS(bmAttributes=2,bEndpointAddress=0x81,read=read)
        interface=Interface([outgoing,endpoint])
        device=NS(bus=2,port_numbers=(4,),get_active_configuration=lambda:[interface],is_kernel_driver_active=lambda _:False)
        backend=object()
        def find(**kwargs):
            self.assertIs(kwargs['backend'],backend)
            self.assertEqual((kwargs['idVendor'],kwargs['idProduct']),(0x0e8d,0x201c))
            return [device]
        usb=NS(core=NS(find=find),util=NS(claim_interface=lambda d,n:claims.append(n),release_interface=lambda d,n:claims.remove(n),dispose_resources=lambda d:None))
        self.serial_opened=[]
        serial=NS(read=read,write=outgoing.write,close=lambda:self.serial_opened.append('closed'))
        def serial_port(selected,usb_module):
            self.serial_opened.append((selected,usb_module is usb))
            return serial
        return StageUsb(usb,backend,Candidate(2,10,(4,),0x0e8d,0x2000),platform=platform,serial_port=serial_port),incoming,writes,claims

    def test_windows_talks_to_the_stage_over_its_serial_port(self):
        stage,incoming,writes,claims=self.fixture(platform='win32')
        self.assertEqual(claims,[])  # the vendor interface has no Windows driver and is never claimed
        self.assertEqual(self.serial_opened,[(Candidate(2,10,(4,),0x0e8d,0x2000),True)])
        self.assertEqual(struct.unpack('<4sIQ',writes[-1]),(b'CBP1',0,0))
        value=json.dumps({'status':'ready'}).encode()
        incoming.extend(struct.pack('<4sIQ',b'CBR1',0,len(value))+value)
        self.assertEqual(stage.dispatch('stage_status',None),{'status':'ready'})
        stage.close()
        self.assertEqual(self.serial_opened[-1],'closed')

    def test_stage_port_resolution_uses_the_retained_physical_port(self):
        from stage_usb import open_stage_port
        import stage_usb
        seen=[]
        def fake_open(candidate,interface_number,usb,**kwargs):
            seen.append((candidate,kwargs['wait'],kwargs['services'](1,2)))
            return 'port'
        original=stage_usb.open_serial_port
        stage_usb.open_serial_port=fake_open
        try:
            self.assertEqual(open_stage_port(Candidate(2,10,(4,1),0x0e8d,0x2000),object()),'port')
        finally:
            stage_usb.open_serial_port=original
        self.assertEqual(seen,[(Candidate(2,0,(4,1),0x0e8d,0x201c),stage_usb.STAGE_PORT_WAIT,[])])

    def test_exact_interface_setup_and_no_flash_opcode(self):
        stage,incoming,writes,claims=self.fixture()
        self.assertEqual(claims,[3])
        count=len(writes)
        with self.assertRaises(InstallError):stage.request(10,b'{}',maximum=0)
        self.assertEqual(len(writes),count)
        value=json.dumps({'status':'ready','scan':True}).encode()
        incoming.extend(struct.pack('<4sIQ',b'CBR1',0,len(value))+value)
        self.assertEqual(stage.dispatch('stage_status',None),{'status':'ready','scan':True})
        self.assertEqual(struct.unpack('<4sIQ',writes[-1]),(b'CBP1',5,0))
        stage.close();self.assertEqual(claims,[])

    def test_oversized_status_and_arbitrary_setup_fields_fail_closed(self):
        stage,incoming,writes,claims=self.fixture()
        incoming.extend(struct.pack('<4sIQ',b'CBR1',0,513))
        with self.assertRaises(InstallError):stage.dispatch('stage_status',None)
        count=len(writes)
        with self.assertRaises(InstallError):stage.dispatch('stage_bind',{'path':'/dev/mmcblk0'})
        with self.assertRaises(InstallError):stage.dispatch('stage_provision',{'ssid_hex':'x'})
        self.assertEqual(len(writes),count)
        stage.close()

    def test_short_setup_write_is_not_retried(self):
        stage,incoming,writes,claims=self.fixture()
        attempts=[]
        stage.outgoing.write=lambda data,**kw:attempts.append(data) or 1
        with self.assertRaises(InstallError):stage.dispatch('stage_scan',None)
        self.assertEqual(len(attempts),1)
        stage.close()

if __name__=='__main__':unittest.main()
