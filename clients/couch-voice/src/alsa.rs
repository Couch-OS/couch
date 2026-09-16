//! Capture, straight onto the ALSA character devices.
//!
//! The flow is the one every ALSA client uses and alsa-lib merely wraps:
//! open `/dev/snd/pcmC<card>D<device>c`, negotiate `hw_params`, set
//! `sw_params`, PREPARE, START, then `readi` in a loop. Doing it by hand costs
//! about four hundred lines; linking libasound costs a cross sysroot, which
//! this project has already decided it will not have.
//!
//! Two departures from what alsa-lib would do, both because this code has to
//! run on hardware nobody has proven yet.
//!
//! The device is opened non-blocking and every read is fronted by `poll` with
//! a deadline. A capture device whose DMA never starts - a plausible outcome
//! if the analogue path is powered down - blocks forever in a blocking
//! `readi`, and a probe that hangs tells you nothing. With a deadline it says
//! "this device produced no frames in two seconds", which is an answer.
//!
//! And there is no `snd_pcm_status`: its struct grew a member after 3.18, so
//! its ioctl number is not the same on every kernel. Overruns are detected the
//! way they are reported anyway, by `readi` returning EPIPE.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::abi::{self, HwParams, Interval, Mask, PcmInfo, SwParams, Xferi};
use crate::error::{Error, Result};
use crate::level::{Level, Meter};
use crate::Source;

/// Home Assistant's pipeline resamples anything else, but 16 kHz is what its
/// speech-to-text stage runs at, so asking for it here is one fewer conversion.
pub const HA_RATE: u32 = 16_000;

/// 64 ms at 16 kHz. Long enough that a 1 GHz A7 is not woken 50 times a second
/// for a remote control, short enough that a level meter driven off it looks
/// live and that releasing the button stops the recording promptly.
pub const DEFAULT_PERIOD: u32 = 1024;

/// Four periods of slack before the kernel drops samples on the floor. A
/// quarter of a second is a lot of scheduling delay to absorb, and the memory
/// is 16 KB.
pub const DEFAULT_PERIODS: u32 = 4;

/// Nothing anyone says to a remote control takes this long, and an
/// open microphone that nobody stopped is the failure this exists to bound.
pub const DEFAULT_LIMIT: Duration = Duration::from_secs(30);

/// What a capture device says it is. `id` and `name` are the useful ones: on
/// a MediaTek ASoC card they carry the DAI link name, which is how you tell
/// the microphone from the Bluetooth uplink without guessing at device numbers.
#[derive(Debug, Clone)]
pub struct Info {
    pub card: i32,
    pub device: u32,
    pub id: String,
    pub name: String,
    pub subname: String,
    pub subdevices: u32,
}

/// What a device will accept, as HW_REFINE reports it with nothing asked for.
#[derive(Debug, Clone)]
pub struct Caps {
    pub access: Vec<u32>,
    pub formats: Vec<u32>,
    pub channels: (u32, u32),
    pub rates: (u32, u32),
    pub period_frames: (u32, u32),
    pub buffer_frames: (u32, u32),
}

/// The configuration a stream actually settled on.
#[derive(Debug, Clone, Copy)]
pub struct Format {
    pub rate: u32,
    pub channels: u32,
    /// The ALSA sample format the hardware gave us. Not always S16_LE, which
    /// is why `Capture` converts.
    pub sample_format: u32,
    pub period_frames: u32,
    pub buffer_frames: u32,
}

impl Format {
    pub fn bytes_per_sample(&self) -> usize {
        match self.sample_format {
            abi::FORMAT_S16_LE => 2,
            abi::FORMAT_S32_LE => 4,
            _ => 0,
        }
    }
}

/// An open PCM device that has not been configured yet.
pub struct Pcm {
    file: File,
    path: String,
}

impl Pcm {
    /// `hw:0,1`, `0,1`, `1` (device 1 of card 0) or a full `/dev/snd/...` path.
    pub fn open(spec: &str) -> Result<Pcm> {
        let path = device_path(spec);
        // Non-blocking from the start: see the module docs. It also means the
        // open itself cannot hang on a driver waiting for a clock.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .map_err(|e| Error::Alsa {
                device: path.clone(),
                call: "open",
                source: e,
            })?;
        Ok(Pcm { file, path })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// The driver's protocol version, as `major.minor.subminor`. Worth
    /// printing in a probe: if this comes back at all, the ioctl numbering in
    /// `abi.rs` is right, because a wrong number is ENOTTY.
    pub fn protocol(&self) -> Result<(u32, u32, u32)> {
        let mut v: i32 = 0;
        self.call(
            "PVERSION",
            abi::PCM_IOCTL_PVERSION,
            &mut v as *mut _ as *mut _,
        )?;
        let v = v as u32;
        Ok(((v >> 16) & 0xffff, (v >> 8) & 0xff, v & 0xff))
    }

    pub fn info(&self) -> Result<Info> {
        let mut info: PcmInfo = unsafe { std::mem::zeroed() };
        self.call("INFO", abi::PCM_IOCTL_INFO, &mut info as *mut _ as *mut _)?;
        Ok(Info {
            card: info.card,
            device: info.device,
            id: abi::cstr(&info.id),
            name: abi::cstr(&info.name),
            subname: abi::cstr(&info.subname),
            subdevices: info.subdevices_count,
        })
    }

    /// What the hardware will take, with nothing constrained.
    pub fn caps(&self) -> Result<Caps> {
        let mut p = HwParams::any();
        self.refine(&mut p)?;
        let list = |m: &Mask, last: u32| -> Vec<u32> { (0..=last).filter(|v| m.has(*v)).collect() };
        let range = |i: &Interval| (i.min, i.max);
        Ok(Caps {
            access: list(p.mask(abi::P_ACCESS), 4),
            formats: list(p.mask(abi::P_FORMAT), 40),
            channels: range(p.interval(abi::P_CHANNELS)),
            rates: range(p.interval(abi::P_RATE)),
            period_frames: range(p.interval(abi::P_PERIOD_SIZE)),
            buffer_frames: range(p.interval(abi::P_BUFFER_SIZE)),
        })
    }

    /// Whether one exact configuration survives refinement. This asks the
    /// driver rather than the buffer: HW_REFINE changes nothing and can be run
    /// against every device on the card in a loop.
    pub fn accepts(&self, rate: u32, channels: u32, sample_format: u32) -> Result<bool> {
        let mut p = HwParams::any();
        p.set_mask(abi::P_ACCESS, Mask::only(abi::ACCESS_RW_INTERLEAVED));
        p.set_mask(abi::P_FORMAT, Mask::only(sample_format));
        p.set_mask(abi::P_SUBFORMAT, Mask::only(abi::SUBFORMAT_STD));
        p.set_interval(abi::P_CHANNELS, Interval::exactly(channels));
        p.set_interval(abi::P_RATE, Interval::exactly(rate));
        match self.refine(&mut p) {
            Ok(()) => Ok(!p.interval(abi::P_RATE).is_empty()),
            // EINVAL from refine is the driver saying no, which is an answer
            // and not a failure.
            Err(Error::Alsa { source, .. }) if source.raw_os_error() == Some(libc::EINVAL) => {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// Negotiate, and hand back a stream that is prepared and running.
    pub fn configure(self, want: &Wanted) -> Result<Capture> {
        let format = self.negotiate(want)?;
        let bytes = format.bytes_per_sample();
        if bytes == 0 {
            return Err(Error::Audio {
                device: self.path.clone(),
                detail: format!(
                    "settled on sample format {} which this client cannot convert; \
                     it handles S16_LE and S32_LE",
                    abi::format_name(format.sample_format)
                ),
            });
        }
        self.set_sw_params(&format)?;

        let frames = format.period_frames as usize;
        let mut capture = Capture {
            pcm: self,
            format,
            raw: vec![0u8; frames * format.channels as usize * bytes],
            limit_frames: (want.limit.as_secs_f64() * format.rate as f64) as u64,
            frames_read: 0,
            overruns: 0,
            silent_polls: 0,
            running: false,
            stop: Arc::new(AtomicBool::new(false)),
            meter: Meter::default(),
        };
        capture.start()?;
        Ok(capture)
    }

    /// Try the caller's exact wish, then loosen it in two steps. Period and
    /// buffer sizes are the parameters a driver is most likely to be fussy
    /// about, and they are also the ones we care least about; the rate,
    /// channel count and sample format are not negotiable here because the
    /// caller has already been told what the device accepts.
    fn negotiate(&self, want: &Wanted) -> Result<Format> {
        let attempts = [
            (Some(want.period), Some(want.period * want.periods)),
            (Some(want.period), None),
            (None, None),
        ];
        let mut last = None;
        for (period, buffer) in attempts {
            let mut p = HwParams::any();
            p.set_mask(abi::P_ACCESS, Mask::only(abi::ACCESS_RW_INTERLEAVED));
            p.set_mask(abi::P_FORMAT, Mask::only(want.sample_format));
            p.set_mask(abi::P_SUBFORMAT, Mask::only(abi::SUBFORMAT_STD));
            p.set_interval(abi::P_CHANNELS, Interval::exactly(want.channels));
            p.set_interval(abi::P_RATE, Interval::exactly(want.rate));
            if let Some(n) = period {
                p.set_interval(abi::P_PERIOD_SIZE, Interval::exactly(n));
            }
            if let Some(n) = buffer {
                p.set_interval(abi::P_BUFFER_SIZE, Interval::between(n, n));
            }
            match self.call(
                "HW_PARAMS",
                abi::PCM_IOCTL_HW_PARAMS,
                &mut p as *mut _ as *mut _,
            ) {
                Ok(_) => {
                    return Ok(Format {
                        rate: p.interval(abi::P_RATE).min,
                        channels: p.interval(abi::P_CHANNELS).min,
                        sample_format: want.sample_format,
                        period_frames: p.interval(abi::P_PERIOD_SIZE).min,
                        buffer_frames: p.interval(abi::P_BUFFER_SIZE).min,
                    })
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.expect("at least one attempt"))
    }

    fn set_sw_params(&self, format: &Format) -> Result<()> {
        let (major, minor, sub) = self.protocol()?;
        let mut sw = SwParams {
            // Wake us when a whole period is ready, which is also what `poll`
            // waits for, so a read never returns a fragment.
            avail_min: format.period_frames as abi::Uframes,
            start_threshold: 1,
            stop_threshold: format.buffer_frames as abi::Uframes,
            // The kernel overwrites this with its own; alsa-lib and tinyalsa
            // both send a plausible value anyway.
            boundary: (format.buffer_frames as abi::Uframes) << 8,
            period_step: 1,
            proto: (major << 16) | (minor << 8) | sub,
            ..SwParams::default()
        };
        self.call(
            "SW_PARAMS",
            abi::PCM_IOCTL_SW_PARAMS,
            &mut sw as *mut _ as *mut _,
        )?;
        Ok(())
    }

    fn refine(&self, p: &mut HwParams) -> Result<()> {
        self.call(
            "HW_REFINE",
            abi::PCM_IOCTL_HW_REFINE,
            p as *mut _ as *mut libc::c_void,
        )?;
        Ok(())
    }

    fn call(&self, name: &'static str, request: u32, arg: *mut libc::c_void) -> Result<i32> {
        unsafe { abi::ioctl(self.file.as_raw_fd(), request, arg) }.map_err(|e| Error::Alsa {
            device: self.path.clone(),
            call: name,
            source: e,
        })
    }
}

/// What a caller wants out of a capture device. Anything the hardware refuses
/// is negotiated down by `Pcm::configure`, except the rate, channels and
/// sample format, which are refused loudly instead of silently substituted -
/// audio at the wrong rate sounds like a fault in something else entirely.
#[derive(Debug, Clone, Copy)]
pub struct Wanted {
    pub rate: u32,
    pub channels: u32,
    pub sample_format: u32,
    pub period: u32,
    pub periods: u32,
    /// The hard cap. Reaching it ends the recording as if the source ran out,
    /// which it did.
    pub limit: Duration,
}

impl Default for Wanted {
    fn default() -> Wanted {
        Wanted {
            rate: HA_RATE,
            // Two, even though one microphone is wanted and the driver will
            // happily agree to one.
            //
            // It agrees and then delivers two anyway. Asking for one produced
            // twice the frames in half the wall clock, and reading L,R,L,R as
            // consecutive mono samples is speech at half speed - and, because
            // the silent right channel interleaves as an alternating zero, a
            // mirror image of the whole spectrum above 4kHz that looks like a
            // hardware fault. Asking for what it actually sends makes the frame
            // accounting right; demux then takes the channel the capsule is on.
            channels: 2,
            sample_format: abi::FORMAT_S16_LE,
            period: DEFAULT_PERIOD,
            periods: DEFAULT_PERIODS,
            limit: DEFAULT_LIMIT,
        }
    }
}

/// A running capture. Dropping it stops the stream and closes the device;
/// there is no way to hold one open without frames flowing into it.
pub struct Capture {
    pcm: Pcm,
    format: Format,
    raw: Vec<u8>,
    limit_frames: u64,
    frames_read: u64,
    overruns: u32,
    silent_polls: u32,
    running: bool,
    stop: Arc<AtomicBool>,
    meter: Meter,
}

/// How long to wait for a period before deciding the device is not producing
/// anything. Generous - three periods, and never less than a second - because
/// a false "no frames" on a working microphone would send someone hunting the
/// wrong fault.
const POLL_SLACK: u32 = 3;

/// Consecutive timeouts before giving up. One timeout can be a scheduling
/// hiccup; four in a row on a device that claimed to start is a dead path.
const MAX_SILENT_POLLS: u32 = 4;

impl Capture {
    pub fn format(&self) -> Format {
        self.format
    }

    /// What the last chunk sounded like. A UI can poll this between reads to
    /// draw a meter; a value that never moves off the floor is the visible
    /// symptom of a microphone that is not wired to anything.
    pub fn level(&self) -> Level {
        self.meter.level()
    }

    pub fn frames(&self) -> u64 {
        self.frames_read
    }

    pub fn overruns(&self) -> u32 {
        self.overruns
    }

    /// A handle that ends the recording from another thread - the button
    /// being released, or a UI closing. Setting it makes the next `read`
    /// return 0, which every consumer already treats as the end.
    pub fn stop_handle(&self) -> StopHandle {
        StopHandle(self.stop.clone())
    }

    fn start(&mut self) -> Result<()> {
        self.pcm
            .call("PREPARE", abi::PCM_IOCTL_PREPARE, std::ptr::null_mut())?;
        self.pcm
            .call("START", abi::PCM_IOCTL_START, std::ptr::null_mut())?;
        self.running = true;
        Ok(())
    }

    /// Stop the stream. Idempotent, and called from `Drop`.
    pub fn stop(&mut self) {
        if self.running {
            let _ = self
                .pcm
                .call("DROP", abi::PCM_IOCTL_DROP, std::ptr::null_mut());
            self.running = false;
        }
    }

    /// Read one period, converted to mono 16-bit. Returns the number of
    /// samples written, or 0 when the limit was reached or somebody stopped it.
    fn read_period(&mut self, out: &mut [i16]) -> Result<usize> {
        if self.stop.load(Ordering::Relaxed) || self.frames_read >= self.limit_frames {
            self.stop();
            return Ok(0);
        }
        let period = self.format.period_frames as usize;
        let channels = self.format.channels as usize;
        if out.len() < period {
            return Err(Error::Audio {
                device: self.pcm.path.clone(),
                detail: format!(
                    "a read buffer of {} is under one {period}-frame period",
                    out.len()
                ),
            });
        }

        let ms =
            (period as u64 * 1000 * POLL_SLACK as u64 / self.format.rate.max(1) as u64).max(1000);
        loop {
            if self.stop.load(Ordering::Relaxed) {
                self.stop();
                return Ok(0);
            }
            if !self.running {
                self.start()?;
            }
            let ready = abi::wait_readable(self.pcm.file.as_raw_fd(), ms as i32).map_err(|e| {
                Error::Alsa {
                    device: self.pcm.path.clone(),
                    call: "poll",
                    source: e,
                }
            })?;
            if !ready {
                self.silent_polls += 1;
                if self.silent_polls >= MAX_SILENT_POLLS {
                    self.stop();
                    return Err(Error::Audio {
                        device: self.pcm.path.clone(),
                        detail: format!(
                            "started but produced no audio in {:.1}s - the capture DMA is not \
                             running, which usually means the analogue path is powered down",
                            (ms * MAX_SILENT_POLLS as u64) as f64 / 1000.0
                        ),
                    });
                }
                continue;
            }

            let mut xfer = Xferi {
                result: 0,
                buf: self.raw.as_mut_ptr() as *mut libc::c_void,
                frames: period as abi::Uframes,
            };
            match unsafe {
                abi::ioctl(
                    self.pcm.file.as_raw_fd(),
                    abi::PCM_IOCTL_READI_FRAMES,
                    &mut xfer as *mut _ as *mut libc::c_void,
                )
            } {
                Ok(_) => {
                    self.silent_polls = 0;
                    let frames = xfer.result.max(0) as usize;
                    if frames == 0 {
                        continue;
                    }
                    let n = self.demux(frames, channels, out);
                    self.frames_read += frames as u64;
                    self.meter.feed(&out[..n]);
                    return Ok(n);
                }
                Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => continue,
                Err(e) if e.raw_os_error() == Some(libc::EPIPE) => {
                    // Overrun: the kernel filled the ring while we were
                    // elsewhere. Recoverable, and worth counting rather than
                    // reporting, because one on a busy device is normal and a
                    // hundred means the buffer is too small.
                    self.overruns += 1;
                    self.running = false;
                    continue;
                }
                Err(e) => {
                    self.stop();
                    return Err(Error::Alsa {
                        device: self.pcm.path.clone(),
                        call: "READI_FRAMES",
                        source: e,
                    });
                }
            }
        }
    }

    /// Interleaved hardware samples to mono `i16`.
    ///
    /// The first channel is taken, not an average of all of them.
    ///
    /// Averaging assumed a two-channel capture was the same signal twice. On
    /// this board it is not: measured over 22 seconds of speech, channel 0 ran
    /// at -35 dBFS rms across 7066 distinct values and channel 1 at -52 dBFS
    /// across 15 - one capsule, wired to the left, and a dead right. Averaging
    /// that in costs 6 dB on a device whose only gain control is already at its
    /// maximum.
    fn demux(&self, frames: usize, channels: usize, out: &mut [i16]) -> usize {
        let width = self.format.bytes_per_sample();
        for (f, sample) in out.iter_mut().take(frames).enumerate() {
            let at = f * channels * width;
            *sample = match width {
                2 => i16::from_le_bytes([self.raw[at], self.raw[at + 1]]),
                _ => {
                    (i32::from_le_bytes([
                        self.raw[at],
                        self.raw[at + 1],
                        self.raw[at + 2],
                        self.raw[at + 3],
                    ]) >> 16) as i16
                }
            };
        }
        frames
    }
}

impl Source for Capture {
    fn rate(&self) -> u32 {
        self.format.rate
    }

    fn chunk_frames(&self) -> usize {
        self.format.period_frames as usize
    }

    fn read(&mut self, out: &mut [i16]) -> Result<usize> {
        self.read_period(out)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The other end of `Capture::stop_handle`.
#[derive(Clone)]
pub struct StopHandle(Arc<AtomicBool>);

impl StopHandle {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Every capture device the kernel is showing, in device order.
///
/// This reads `/dev/snd` rather than `/proc/asound`, because the node is what
/// we are going to open and a node that is not there is the failure we want to
/// report. On a system with no devtmpfs - which is this one - the set of nodes
/// is whatever `mdev -s` made, so a missing device here can also mean the node
/// was never created.
pub fn capture_devices() -> io::Result<Vec<(u32, u32)>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/dev/snd")? {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        // pcmC<card>D<device>c
        let Some(rest) = name.strip_prefix("pcmC") else {
            continue;
        };
        let Some(rest) = rest.strip_suffix('c') else {
            continue;
        };
        let Some((card, device)) = rest.split_once('D') else {
            continue;
        };
        if let (Ok(card), Ok(device)) = (card.parse(), device.parse()) {
            found.push((card, device));
        }
    }
    found.sort_unstable();
    Ok(found)
}

/// `hw:0,1`, `0,1`, `1` or a path, to a path.
pub fn device_path(spec: &str) -> String {
    if spec.starts_with('/') {
        return spec.to_string();
    }
    let spec = spec.strip_prefix("hw:").unwrap_or(spec);
    let (card, device) = match spec.split_once(',') {
        Some((c, d)) => (c, d),
        None => ("0", spec),
    };
    format!("/dev/snd/pcmC{card}D{device}c")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_specs_all_reach_the_same_node() {
        assert_eq!(device_path("hw:0,1"), "/dev/snd/pcmC0D1c");
        assert_eq!(device_path("0,1"), "/dev/snd/pcmC0D1c");
        assert_eq!(device_path("1"), "/dev/snd/pcmC0D1c");
        assert_eq!(device_path("hw:2,16"), "/dev/snd/pcmC2D16c");
        assert_eq!(device_path("/dev/snd/pcmC0D1c"), "/dev/snd/pcmC0D1c");
    }
}
