"""Boot selection fixtures: temporary files only; reboot is intercepted."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / 'stage2/runtime-boot.sh'

@unittest.skipUnless(sys.platform.startswith('linux'), 'Boot fixture runs on Linux')
class RuntimeBootTests(unittest.TestCase):
    def test_updated_slot_mounts_vendor_payload_from_immutable_base(self):
        # Execute the real selection block; the mount helper only records arguments.
        hardware = SCRIPT.with_name('hardware-init.sh').read_text()
        selection = hardware[hardware.index('BUNDLE='):hardware.index('echo "= vendor blobs:')]
        with tempfile.TemporaryDirectory(prefix='couch-vendor-slot-') as temporary:
            root = Path(temporary)
            base = root/'base'; (base/'vendor/lib/modules').mkdir(parents=True)
            (base/'system').mkdir()
            slot = root/'runtime/slots/candidate'; slot.mkdir(parents=True)
            calls = root/'calls'
            helper = root/'mount-fixture'
            helper.write_text('#!/bin/sh\necho "$*" >> "'+str(calls)+'"\n')
            helper.chmod(0o755)
            script = slot/'stage2.sh'
            script.write_text('BB='+str(helper)+'\n'+selection.replace('/mnt/alpine/opt/couch',str(base)))
            subprocess.run(['sh',str(script)],check=True,capture_output=True,timeout=5)
            self.assertEqual(calls.read_text().splitlines(),[
                f'mount -o bind {base}/vendor /vendor',
                f'mount -o bind {base}/system /system'])

    def run_boot(self, previous='base', attempted=True, switched=True):
        with tempfile.TemporaryDirectory(prefix='couch-boot-test-') as temporary:
            root = Path(temporary)
            runtime = root / 'runtime'; runtime.mkdir()
            candidate = 'a' * 64
            slot = runtime / 'slots' / candidate; slot.mkdir(parents=True)
            (slot / 'stage2.sh').write_text('echo candidate\n')
            (root / 'stage2.sh').write_text('echo base\n')
            if previous != 'base':
                old = runtime / 'slots' / previous; old.mkdir()
                (old / 'stage2.sh').write_text('echo previous\n')
            (runtime / 'pending').write_text(f'{previous} {candidate}\n')
            if switched:
                (runtime / 'current').symlink_to('slots/' + candidate)
            if attempted:
                (runtime / 'attempted').write_text(candidate)
            helper = root / 'helper'
            helper.write_text('#!/bin/sh\ncase "$1" in sync) exit 0;; reboot) exit 99;; esac\nexec "$@"\n')
            helper.chmod(0o755)
            script = root / 'runtime-boot.sh'
            script.write_text(SCRIPT.read_text().replace('BB=/bin/busybox', 'BB=' + str(helper)))
            result = subprocess.run(['sh', str(script)], capture_output=True, text=True, timeout=5, check=True)
            return result.stdout.strip(), (runtime / 'pending').exists()

    def boot_health(self, pattern):
        with tempfile.TemporaryDirectory(prefix='couch-health-test-') as temporary:
            root = Path(temporary)
            runtime = root / 'runtime'
            candidate = 'a' * 64
            slot = runtime / 'slots' / candidate
            slot.mkdir(parents=True)
            (slot / 'stage2.sh').write_text('echo candidate\n')
            service = slot / 'couch-system'
            service.write_text('#!/bin/sh\nsleep 10\n' if pattern == 'hung' else '#!/bin/sh\nexit 0\n')
            service.chmod(0o755)
            (root / 'stage2.sh').write_text('echo base\n')
            (runtime / 'current').symlink_to('slots/' + candidate)
            (runtime / 'pending').write_text(f'base {candidate}\n')
            (root / 'proc' / '123').mkdir(parents=True)
            (root / 'proc' / '124').mkdir()
            (root / 'proc' / 'uptime').write_text('100.0 0.0\n')
            helper = root / 'helper'
            helper.write_text(f"""#!/bin/sh
case "$1" in
sync) exit 0;;
timeout) shift 2; exec timeout 0.01 "$@";;
reboot) touch '{root}/rebooted'; exit 0;;
sleep)
 n=$(cat '{root}/ticks' 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > '{root}/ticks'
 now=$((100+n)); echo "$now.0 0.0" > '{root}/proc/uptime'
 if [ '{pattern}' = frozen ]; then
  echo '123 100' > '{root}/health'
 elif [ '{pattern}' = changing ]; then
  echo "$((123+n%2)) $now" > '{root}/health'
 elif [ '{pattern}' = healthy ] || [ '{pattern}' = hung ] || {{ [ '{pattern}' = intermittent ] && [ $((n%3)) -ne 0 ]; }}; then
  echo "123 $now" > '{root}/health'
 else
  rm -f '{root}/health'
 fi
 exit 0;;
esac
exec "$@"
""")
            helper.chmod(0o755)
            script = root / 'runtime-boot.sh'
            script.write_text(SCRIPT.read_text()
                              .replace('BB=/bin/busybox', 'BB=' + str(helper))
                              .replace('/tmp/couch-gui.health', str(root / 'health'))
                              .replace('/tmp/update-boot.log', str(root / 'boot.log'))
                              .replace('/proc/', str(root / 'proc') + '/'))
            result = subprocess.run(['sh', str(script)], capture_output=True, text=True, timeout=5, check=True)
            import time
            deadline = time.monotonic() + 5
            while (runtime / 'pending').exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertFalse((runtime / 'pending').exists(), (root / 'boot.log').read_text())
            # The watcher may remove pending immediately before writing rebooted.
            if pattern != 'healthy':
                while not (root / 'rebooted').exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
            return (result.stdout.strip(), (runtime / 'current').is_symlink(),
                    (root / 'rebooted').exists(),
                    (runtime / 'previous').read_text().strip() if (runtime / 'previous').exists() else None)

    def test_healthy_candidate_is_committed_after_consecutive_checks(self):
        self.assertEqual(self.boot_health('healthy'), ('candidate', True, False, 'base'))

    def test_frozen_gui_with_live_pid_cannot_commit_recent_heartbeat(self):
        self.assertEqual(self.boot_health('frozen'), ('candidate', False, True, None))

    def test_restarting_gui_cannot_accumulate_heartbeats_from_different_processes(self):
        self.assertEqual(self.boot_health('changing'), ('candidate', False, True, None))

    def test_hung_system_health_rolls_back_and_requests_reboot(self):
        self.assertEqual(self.boot_health('hung'), ('candidate', False, True, None))

    def test_missing_heartbeat_rolls_back_and_requests_reboot(self):
        self.assertEqual(self.boot_health('missing'), ('candidate', False, True, None))

    def test_intermittent_heartbeat_does_not_accumulate_successes(self):
        self.assertEqual(self.boot_health('intermittent'), ('candidate', False, True, None))

    def test_failed_candidate_returns_to_factory_runtime(self):
        self.assertEqual(self.run_boot(), ('base', False))

    def test_failed_candidate_returns_to_previous_slot(self):
        self.assertEqual(self.run_boot(previous='b' * 64), ('previous', False))

    def test_power_loss_before_pointer_switch_keeps_current_runtime(self):
        self.assertEqual(self.run_boot(attempted=False, switched=False), ('base', False))

if __name__ == '__main__':
    unittest.main()
