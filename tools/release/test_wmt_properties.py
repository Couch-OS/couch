from pathlib import Path
import subprocess
import tempfile
import unittest


class WmtPropertiesTests(unittest.TestCase):
    def test_only_detected_ha100_transport_satisfies_launcher_properties(self):
        source = Path(__file__).resolve().parents[1]/'installer/wifi-stage/wmt-properties.c'
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = root/'fixture.c'
            fixture.write_text(r'''
#include <assert.h>
#include <string.h>
static int chip = 0x6580, available = 1;
int open(const char *p, int flags, ...) { (void)p; (void)flags; return available ? 8 : -1; }
int close(int fd) { (void)fd; return 0; }
int ioctl(int fd, unsigned long op, ...) { assert(fd == 8); assert(op == 0x80047700UL); return chip; }
int property_get(const char *, char *, const char *);
int main(void) {
 char value[92];
 assert(property_get("service.wcn.driver.ready", value, 0) == 3); assert(!strcmp(value,"yes"));
 assert(property_get("persist.mtk.wcn.combo.chipid", value, 0) == 8); assert(!strcmp(value,"00006580"));
 assert(property_get("unrelated", value, "default") == 7); assert(!strcmp(value,"default"));
 chip = -1; assert(property_get("service.wcn.driver.ready", value, 0) == 0);
 chip = 0x9999; assert(property_get("persist.mtk.wcn.combo.chipid", value, 0) == 0);
 chip = 0x6580; available = 0; assert(property_get("service.wcn.driver.ready", value, 0) == 0);
 return 0;
}
''')
            subprocess.run(['cc','-Wall','-Wextra','-Werror',str(source),str(fixture),'-o',str(root/'test')],check=True,capture_output=True)
            subprocess.run([str(root/'test')],check=True,capture_output=True,timeout=5)
