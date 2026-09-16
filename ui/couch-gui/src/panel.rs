//! The panel: framebuffer output and the Slint platform that drives it.
//!
//! This kernel has no DRM/KMS and no X11 or Wayland, so none of Slint's stock
//! backends apply. We supply a Platform and render into /dev/fb0 ourselves.
//!
//! Two details are load-bearing, both learned the hard way:
//!
//! - The panel reads the low byte as red (fb_var_screeninfo reports red=0/8),
//!   so the buffer is ABGR in memory. Declaring that through TargetPixel means
//!   the renderer writes it directly, instead of a swizzle pass over every
//!   pixel of every frame.
//! - Draw into cached RAM and copy only the dirty rectangles to the mapped
//!   framebuffer. Writing the framebuffer directly during rasterisation is what
//!   made an earlier version cost 98ms a frame instead of one.

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel,
};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::PhysicalSize;

/// Memory order is B,G,R,A here because the panel takes red in the low byte.
#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Default)]
pub struct Abgr(pub u32);

impl TargetPixel for Abgr {
    fn blend(&mut self, c: PremultipliedRgbaColor) {
        let a = (255 - c.alpha) as u32;
        let (r, g, b) = (self.0 & 0xff, (self.0 >> 8) & 0xff, (self.0 >> 16) & 0xff);
        self.0 = 0xff00_0000
            | ((c.blue as u32 + (b * a) / 255) << 16)
            | ((c.green as u32 + (g * a) / 255) << 8)
            | (c.red as u32 + (r * a) / 255);
    }
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Abgr(0xff00_0000 | ((b as u32) << 16) | ((g as u32) << 8) | r as u32)
    }
}

// fb_var_screeninfo is 40 u32s: xres, yres, xres_virtual, yres_virtual,
// xoffset, yoffset, ... The driver wants the whole struct back for a pan.
const FBIOGET_VSCREENINFO: libc::c_int = 0x4600;
const FBIOPAN_DISPLAY: libc::c_int = 0x4606;
/// FBIOBLANK: 0 unblanks, 4 is FB_BLANK_POWERDOWN. On mtkfb that suspends the
/// LCM, cuts the backlight PWM and suspends the touch controller (~570ms),
/// and unblanking re-initialises the panel (~430ms) - measured in dmesg.
const FBIOBLANK: libc::c_int = 0x4611;
const FB_BLANK_UNBLANK: libc::c_int = 0;
const FB_BLANK_POWERDOWN: libc::c_int = 4;
// _IOW('F', 0x20, __u32) on 32-bit ARM.
const FBIO_WAITFORVSYNC: libc::c_int = 0x4004_4620;

/// One frame period on the 60Hz panel, for the timed pacing.
const FRAME: Duration = Duration::from_micros(16_667);
/// A pacing wait longer than this is a stall, and the ioctl that did it is
/// abandoned for the timed sleep.
const STALL: Duration = Duration::from_millis(100);

/// How each drawn frame is held back to the panel's refresh.
///
/// Without this the loop renders back-to-back for as long as anything
/// animates: ~600 frames per five seconds under COUCH_NAV, of which the panel
/// could show 300. Every one of those cost a rasterisation.
#[derive(Copy, Clone, PartialEq, Debug)]
enum Pacing {
    /// FBIO_WAITFORVSYNC: the driver blocks until the next vertical sync.
    WaitForVsync,
    /// FBIOPAN_DISPLAY with a zero offset. A pan blocks ~17ms waiting for
    /// vsync on an idle panel and changes nothing about what is shown - but
    /// while a key is held it blocks for the whole hold, in the kernel, with
    /// nothing else running. Opt-in only; see probe_pacing.
    Pan,
    /// Sleep until one frame after the draw began. The default: no kernel
    /// wait to go wrong, and at 60Hz nobody can tell it from vsync.
    Sleep,
}

/// What a frame cost, in microseconds. `work` is rasterising plus the copy to
/// the panel - the cost that would exist at any refresh rate. `wait` is the
/// pacing after it, which is slack, not work.
pub struct FrameCost {
    pub work_us: u64,
    pub wait_us: u64,
}

/// Where the page arriving in a transition comes from. What is on screen
/// leaves the other way: a page from the right pushes the old one off to the
/// left. The next area is to the right, so is the chooser; going back is from
/// the left.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Arrive {
    FromRight,
    FromLeft,
}

/// What a transition cost, summed over its frames, in the same terms as
/// `FrameCost` so the loop can fold it into the same statistics.
#[derive(Default)]
pub struct SlideCost {
    pub frames: u64,
    pub work_us: u64,
    pub wait_us: u64,
    pub max_us: u64,
}

/// How long a page takes to cross.
pub const SLIDE: Duration = Duration::from_millis(180);

pub struct Panel {
    fb: File,
    map: &'static mut [u32],
    pub width: u32,
    pub height: u32,
    stride_px: u32,
    ram: Vec<Abgr>,
    /// The frame the panel showed when a transition began: page A. RAM is
    /// page B by then, so the two pages of a slide are this and `ram`.
    spare: Vec<Abgr>,
    /// The screeninfo the driver reported, offsets zeroed, for FBIOPAN_DISPLAY.
    var: Option<[u32; 40]>,
    pacing: Pacing,
    /// The panel is powered down (standby). Frames still rasterise into RAM
    /// so the picture is current the moment it comes back; nothing is copied
    /// to a panel that is not showing it.
    blanked: bool,
    core_floor: crate::core_floor::CoreFloor,
}

/// Keypad backlight policy, see `Panel::set_keys_policy`. Defaults keep the
/// keys lit at the full-brightness level until the settings are loaded.
static KEYS_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static KEYS_ACTIVE_LEVEL: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(255);
impl Panel {
    pub fn open() -> std::io::Result<Self> {
        let fb = File::options().read(true).write(true).open("/dev/fb0")?;

        // Ask the driver, not sysfs: /sys/class/graphics/fb0/virtual_size
        // reports the virtual extent (480x2400 here, three pages of
        // scrollback), and rendering to that means drawing three screens per
        // frame. fb_var_screeninfo starts xres, yres, xres_virtual, ...
        let mut vinfo = [0u32; 40];
        let rc = unsafe { libc::ioctl(fb.as_raw_fd(), FBIOGET_VSCREENINFO as _, vinfo.as_mut_ptr()) };
        let var = (rc == 0 && vinfo[0] > 0 && vinfo[1] > 0).then_some(vinfo);
        let (width, height) = var.map_or((480, 800), |v| (v[0], v[1]));
        let stride = std::fs::read_to_string("/sys/class/graphics/fb0/stride")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(width * 4);

        let len = (stride * height) as usize;
        let ptr = unsafe {
            libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE,
                       libc::MAP_SHARED, fb.as_raw_fd(), 0)
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        let map = unsafe { std::slice::from_raw_parts_mut(ptr as *mut u32, len / 4) };

        let mut panel = Panel {
            fb,
            map,
            width,
            height,
            stride_px: stride / 4,
            ram: vec![Abgr::default(); (width * height) as usize],
            spare: vec![Abgr::default(); (width * height) as usize],
            // Page 0 is what is displayed; the pan must say so.
            var: var.map(|mut v| { v[4] = 0; v[5] = 0; v }),
            pacing: Pacing::Sleep,
            blanked: false,
            core_floor: crate::core_floor::CoreFloor::new("/proc/hps/num_base_perf_serv"),
        };
        panel.pacing = panel.probe_pacing();
        Ok(panel)
    }

    /// Pick the wait once, at startup, and say which.
    ///
    /// Each ioctl is tried twice and the second call timed: the first may
    /// return at once if a sync happens to be due, but the second must block
    /// for a whole period. A driver that accepts the ioctl and returns without
    /// waiting would otherwise pass as vsync and pace nothing.
    ///
    /// COUCH_VSYNC=0 (or `sleep`) forces the timed fallback so the modes can be
    /// compared on the device; `pan` skips FBIO_WAITFORVSYNC; `wait` skips the
    /// pan.
    fn probe_pacing(&mut self) -> Pacing {
        let choice = std::env::var("COUCH_VSYNC").unwrap_or_default();
        // The timed sleep is the default, not the fallback. Both ioctls were
        // measured to work on an idle panel, and FBIOPAN_DISPLAY then stalled
        // the whole kernel for as long as any key was held: 5.3s of system
        // time inside cmdqCoreWaitResultAndReleaseTask on a single online
        // core, nothing else scheduled, the microphone thread starved to a
        // 0.1s recording. The keypad rescans every 8ms while a key is down
        // and something in that upsets the display's command queue. Not
        // ours to fix; opt in with COUCH_VSYNC=auto|pan|wait to experiment.
        let (try_wait, try_pan) = match choice.as_str() {
            "auto" => (true, true),
            "pan" => (false, true),
            "wait" => (true, false),
            _ => (false, false),
        };
        let mut why = Vec::new();
        if !try_wait && !try_pan {
            why.push(if choice.is_empty() {
                "default; the ioctls stall while a key is held".to_string()
            } else {
                format!("COUCH_VSYNC={choice}")
            });
        }

        let fd = self.fb.as_raw_fd();
        if try_wait {
            if let Some(period) = probe("FBIO_WAITFORVSYNC", &mut why, &mut || wait_for_vsync(fd)) {
                println!("couch-gui: pacing: FBIO_WAITFORVSYNC, {:.1}ms per wait{}",
                         period.as_secs_f64() * 1e3, reasons(&why));
                return Pacing::WaitForVsync;
            }
        }
        if try_pan {
            match self.var {
                Some(mut var) => {
                    if let Some(period) =
                        probe("FBIOPAN_DISPLAY", &mut why, &mut || pan_display(fd, &mut var))
                    {
                        println!("couch-gui: pacing: FBIOPAN_DISPLAY, {:.1}ms per wait{}",
                                 period.as_secs_f64() * 1e3, reasons(&why));
                        return Pacing::Pan;
                    }
                }
                None => why.push("FBIOPAN_DISPLAY: no screeninfo".into()),
            }
        }
        println!("couch-gui: pacing: sleep to {:.2}ms{}", FRAME.as_secs_f64() * 1e3, reasons(&why));
        Pacing::Sleep
    }

    /// Take the panel over from fbcon and clear it.
    ///
    /// The marker stops fbcon painting; init's console keeps draining its pipe
    /// but stops drawing, so boot output cannot land on top of the UI. Clearing
    /// matters because the renderer only ever repaints what changed, so
    /// whatever the console left behind would otherwise survive under us.
    pub fn claim(&mut self, background: u32) {
        let _ = std::fs::write("/tmp/couch.gui", b"");
        let bg = 0xff00_0000
            | ((background & 0x0000ff) << 16)
            | (background & 0x00ff00)
            | ((background >> 16) & 0x0000ff);
        for px in self.map.iter_mut() {
            *px = bg;
        }
        for px in self.ram.iter_mut() {
            *px = Abgr(bg);
        }
    }

    /// Panel and key backlights. init used to rewrite 255 to both every five
    /// seconds; stage2 stops that loop so the levels set here stick. The key
    /// LEDs are all or nothing: lit only at full brightness.
    ///
    /// A write that matches the value the LED node already holds is dropped
    /// by the LED layer before it reaches the display driver (measured: 10ms
    /// and no driver call, against 110ms and a PWM change otherwise). That is
    /// harmless until the driver below it silently loses a write - seen once
    /// on a wake from dim: the node said 255, the PWM stayed at the dim level,
    /// and every later 255 was deduplicated away. So the panel was stuck dim
    /// until something wrote a different value. Writing a neighbour first
    /// when the node already holds the target defeats that, at the cost of a
    /// second driver call only in the case that would otherwise be stuck.
    /// Whether the keypad backlight is wanted at all, and the level that means
    /// "awake": the keys are lit only for that level, never while dimmed.
    pub fn set_keys_policy(enabled: bool, active_level: u8) {
        KEYS_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
        KEYS_ACTIVE_LEVEL.store(active_level, std::sync::atomic::Ordering::Relaxed);
        let keys = enabled && active_level > 0 && Self::current_backlight() == Some(active_level);
        Self::set_keys(keys);
    }
    fn current_backlight() -> Option<u8> {
        std::fs::read_to_string("/sys/class/leds/lcd-backlight/brightness")
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }
    fn set_keys(lit: bool) {
        let _ = std::fs::write(
            "/sys/class/leds/button-backlight/brightness",
            if lit { "255\n" } else { "0\n" },
        );
    }
    /// What the key LED node holds right now, if it can be read.
    fn keys_lit() -> Option<bool> {
        std::fs::read_to_string("/sys/class/leds/button-backlight/brightness")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .map(|v| v > 0)
    }
    /// Hold the key LEDs to `lit`, correcting a state that changed under us.
    /// The GUI only writes the node on its own transitions, and something
    /// else - the keypad driver on a key press, most likely - can light it
    /// while the screen is off. Called once a second; returns the state that
    /// was found when a correction was needed, so the log can say so.
    pub fn enforce_keys(lit: bool) -> Option<bool> {
        match Self::keys_lit() {
            Some(found) if found != lit => {
                Self::set_keys(lit);
                Some(found)
            }
            _ => None,
        }
    }
    pub fn set_backlight(level: u8) {
        const LCD: &str = "/sys/class/leds/lcd-backlight/brightness";
        let held: Option<u8> = std::fs::read_to_string(LCD)
            .ok()
            .and_then(|s| s.trim().parse().ok());
        if held == Some(level) {
            let nudge = if level == 255 { 254 } else { level + 1 };
            let _ = std::fs::write(LCD, format!("{nudge}\n"));
        }
        if let Err(e) = std::fs::write(LCD, format!("{level}\n")) {
            println!("couch-gui: backlight {level}: {e}");
        }
        // The key LEDs are a GPIO, lit or not: lit while the screen is at its
        // awake level and the setting wants them, dark while dimmed or off.
        let awake = KEYS_ACTIVE_LEVEL.load(std::sync::atomic::Ordering::Relaxed);
        let wanted = KEYS_ENABLED.load(std::sync::atomic::Ordering::Relaxed);
        Self::set_keys(wanted && level > 0 && level == awake);
    }

    /// The backlight, delivered for certain: a neighbouring value first, then
    /// the level, so both reach the driver whatever the LED node holds. Two
    /// driver calls, ~220ms of the display's command queue; used once, a
    /// second after a wake, to catch a write the driver dropped.
    pub fn force_backlight(level: u8) {
        const LCD: &str = "/sys/class/leds/lcd-backlight/brightness";
        let nudge = if level == 255 { 254 } else { level + 1 };
        let _ = std::fs::write(LCD, format!("{nudge}\n"));
        let _ = std::fs::write(LCD, format!("{level}\n"));
    }

    pub fn backlight_on() {
        Panel::set_backlight(255);
    }

    /// Power the panel down or back up. Coming back, the whole picture is
    /// pushed from RAM: the panel was re-initialised and RAM is the truth.
    pub fn blank(&mut self, off: bool) {
        // Restore the active floor before wake/rendering. Repeated calls also
        // retry a failed floor write, without repeating the display ioctl.
        if !off || off == self.blanked {
            if let Err(error) = self.core_floor.display_off(off) {
                println!("couch-gui: CPU floor: {error}");
            }
        }
        if off == self.blanked {
            return;
        }
        let arg = if off { FB_BLANK_POWERDOWN } else { FB_BLANK_UNBLANK };
        let rc = unsafe { libc::ioctl(self.fb.as_raw_fd(), FBIOBLANK as _, arg as libc::c_ulong) };
        if rc != 0 {
            println!("couch-gui: FBIOBLANK({arg}) failed: {}", std::io::Error::last_os_error());
            return;
        }
        self.blanked = off;
        if off {
            if let Err(error) = self.core_floor.display_off(true) {
                println!("couch-gui: standby CPU floor: {error}");
            }
        }
        if !off {
            self.refresh_all();
            self.present();
        }
    }

    /// Make the display engine show the framebuffer again.
    ///
    /// After a resume the LCM is lit and scanning, but the overlay it scans
    /// was configured by the last FBIOPAN_DISPLAY - Android re-presents after
    /// every resume, and nothing here did. Measured: framebuffer full of UI,
    /// driver reporting Alive, backlight up, panel black. A pan with zero
    /// offsets re-applies the layer with this buffer. It blocks until the
    /// next refresh (~17ms), longer while a key is held, which a wake is
    /// prepared to pay once.
    pub fn present(&mut self) {
        let fd = self.fb.as_raw_fd();
        if let Some(var) = self.var.as_mut() {
            if let Err(e) = pan_display(fd, var) {
                println!("couch-gui: present: FBIOPAN_DISPLAY failed with {}", errno_name(e));
            }
        }
    }

    /// Whether the display driver has the panel asleep, whoever put it there.
    ///
    /// The GUI's own `blanked` flag only knows what the GUI asked for. Once,
    /// with the GUI merely dimmed, the driver reported `State=Sleep` and
    /// refused every backlight write - the wake wrote 255 and the panel stayed
    /// black. Nothing in this process had blanked it. The driver's debugfs
    /// status is the only place the truth is readable, so a wake asks it and
    /// unblanks if it must. Unreadable debugfs reads as "not asleep".
    pub fn display_asleep() -> bool {
        std::fs::read_to_string("/sys/kernel/debug/mtkfb")
            .map(|s| s.contains("State=Sleep"))
            .unwrap_or(false)
    }

    /// Unblank whether or not this process blanked, when the driver says the
    /// panel is asleep. The GUI's flag is reconciled to what the driver did.
    pub fn unblank_if_asleep(&mut self) -> bool {
        if !self.blanked && !Panel::display_asleep() {
            return false;
        }
        self.blanked = true;
        self.blank(false);
        true
    }

    /// Copy every row of RAM to the panel.
    pub fn refresh_all(&mut self) {
        let (w, h, stride) = (self.width as usize, self.height as usize, self.stride_px as usize);
        let src = pixels(&self.ram);
        for y in 0..h {
            self.map[y * stride..y * stride + w].copy_from_slice(&src[y * w..(y + 1) * w]);
        }
    }

    /// Render one frame if anything changed, then hold until the panel has
    /// had a refresh. Returns what it cost, or None if nothing needed drawing.
    ///
    /// The copy lands first and the wait comes after it, so the copy itself is
    /// not synchronised to blanking - a rectangle can straddle the scan-out.
    /// Copying into the blanking interval instead would need the wait before
    /// the copy, and the region kept across it; acceptable as is for now.
    pub fn render(&mut self, window: &MinimalSoftwareWindow) -> Option<FrameCost> {
        let started = Instant::now();
        if !self.draw(window, true) {
            return None;
        }
        let work = started.elapsed();
        self.pace(started);
        Some(FrameCost {
            work_us: work.as_micros() as u64,
            wait_us: started.elapsed().saturating_sub(work).as_micros() as u64,
        })
    }

    /// Render one frame into RAM and stop there: nothing reaches the panel and
    /// nothing waits for it. Page B of a transition. Returns the cost in
    /// microseconds, or None if nothing needed drawing.
    ///
    /// The renderer's partial redraw is still fine here: RAM holds the last
    /// frame it drew, and the region it marks dirty is what the state change
    /// touched. Until the transition that follows has finished, RAM and the
    /// panel disagree - that is the only time they do.
    pub fn render_offscreen(&mut self, window: &MinimalSoftwareWindow) -> Option<u64> {
        let started = Instant::now();
        self.draw(window, false).then(|| started.elapsed().as_micros() as u64)
    }

    /// Keep the frame the panel is showing: page A of a transition. Taken
    /// before any state changes, because RAM only equals the panel while
    /// nothing has been drawn since the last copy.
    pub fn snapshot(&mut self) {
        self.spare.copy_from_slice(&self.ram);
    }

    /// Rasterise into RAM if anything changed and, if asked, copy the dirty
    /// rectangles to the panel. True if something was drawn.
    ///
    /// COUCH_REGION reports what the renderer marked dirty. A frame that costs
    /// far more than its content suggests is almost always claiming a much
    /// larger region than it needs, and that is invisible without this.
    fn draw(&mut self, window: &MinimalSoftwareWindow, to_panel: bool) -> bool {
        let report = std::env::var_os("COUCH_REGION").is_some();
        let (w, h, stride_px) = (self.width, self.height, self.stride_px);
        let blanked = self.blanked;
        let (ram, map) = (&mut self.ram, &mut *self.map);
        window.draw_if_needed(|renderer| {
            let region = renderer.render(ram, w as usize);
            if report {
                let (mut n, mut px) = (0u32, 0u64);
                // The geometry, not just the total: a frame that costs far more
                // than the moving element explains is claiming something else,
                // and which rectangle it is names the culprit.
                let mut where_ = String::new();
                for (pos, sz) in region.iter() {
                    n += 1;
                    px += (sz.width * sz.height) as u64;
                    where_.push_str(&format!(
                        " [{},{} {}x{}]", pos.x, pos.y, sz.width, sz.height
                    ));
                }
                println!("couch-gui: region {n} rect(s), {px} px = {}% of screen{where_}",
                         px * 100 / (w as u64 * h as u64));
            }
            if !to_panel || blanked {
                return;
            }
            // The region's own rectangles, not its bounding box: a change at
            // opposite ends of the screen has a bounding box of nearly the
            // whole panel.
            let src_px = pixels(ram);
            for (pos, size) in region.iter() {
                let (rx, ry) = (pos.x.max(0) as u32, pos.y.max(0) as u32);
                let n = size.width.min(w.saturating_sub(rx)) as usize;
                if n == 0 {
                    continue;
                }
                for y in ry..(ry + size.height).min(h) {
                    let src = (y * w + rx) as usize;
                    let dst = (y * stride_px + rx) as usize;
                    map[dst..dst + n].copy_from_slice(&src_px[src..src + n]);
                }
            }
        })
    }

    /// Slide page B in over page A, on the panel, without rendering anything.
    ///
    /// Call `snapshot` with the old page showing, change the state, and
    /// `render_offscreen` the new one; then this composes each frame from
    /// those two buffers - one or two copies per row - and paces it to the
    /// refresh the way a drawn frame is. A rasterised slide cost 10-29ms a
    /// frame, both pages redrawn every time; a copy of the panel is about a
    /// millisecond, at any clock.
    ///
    /// `keep` is row bands (y, height) that do not travel and are taken from
    /// B throughout: the status bar, and for an area change the pager, which
    /// is the control and should not move with the thing it controls. The
    /// last frame is all of B, so when this returns the panel and RAM agree
    /// again and the normal path carries on from B.
    pub fn slide(&mut self, from: Arrive, keep: &[(u32, u32)], duration: Duration) -> SlideCost {
        let report = std::env::var_os("COUCH_REGION").is_some();
        let duration = duration.max(FRAME).as_secs_f32();
        let began = Instant::now();
        let mut cost = SlideCost::default();
        loop {
            let started = Instant::now();
            // A frame composed now reaches the glass at the next refresh, so
            // it shows where the page will be one period from now rather than
            // where it is - the first frame moves instead of repeating what
            // is already on the panel.
            let t = ((started.duration_since(began) + FRAME).as_secs_f32() / duration).min(1.0);
            let dx = if t >= 1.0 {
                self.width as usize
            } else {
                (ease_out(t) * self.width as f32).round() as usize
            };
            self.compose(from, dx, keep);
            let work = started.elapsed();
            self.pace(started);
            let wait = started.elapsed().saturating_sub(work);
            let (work_us, wait_us) = (work.as_micros() as u64, wait.as_micros() as u64);
            cost.frames += 1;
            cost.work_us += work_us;
            cost.wait_us += wait_us;
            cost.max_us = cost.max_us.max(work_us);
            if report {
                println!("couch-gui: slide frame {}: dx {dx}, {work_us} us, {wait_us} us paced",
                         cost.frames);
            }
            if t >= 1.0 {
                return cost;
            }
        }
    }

    /// One transition frame straight into the framebuffer: A shifted `dx`
    /// pixels out of the way and B filling what it uncovered, except the
    /// `keep` bands, which are B as they stand.
    fn compose(&mut self, from: Arrive, dx: usize, keep: &[(u32, u32)]) {
        let (w, h, stride) = (self.width as usize, self.height as usize, self.stride_px as usize);
        let dx = dx.min(w);
        let (a, b) = (pixels(&self.spare), pixels(&self.ram));
        for y in 0..h {
            let dst = &mut self.map[y * stride..y * stride + w];
            let (ra, rb) = (&a[y * w..(y + 1) * w], &b[y * w..(y + 1) * w]);
            let held = keep.iter().any(|&(top, height)| {
                y >= top as usize && y < (top + height) as usize
            });
            if held {
                dst.copy_from_slice(rb);
                continue;
            }
            match from {
                Arrive::FromRight => {
                    dst[..w - dx].copy_from_slice(&ra[dx..]);
                    dst[w - dx..].copy_from_slice(&rb[..dx]);
                }
                Arrive::FromLeft => {
                    dst[dx..].copy_from_slice(&ra[..w - dx]);
                    dst[..dx].copy_from_slice(&rb[w - dx..]);
                }
            }
        }
    }

    /// Block until the panel has refreshed. An ioctl that worked at startup
    /// and fails now is not retried: the loop must never spin unpaced. A
    /// signal cutting one wait short is not a failure.
    fn pace(&mut self, started: Instant) {
        let fd = self.fb.as_raw_fd();
        let before = Instant::now();
        let failed = match (self.pacing, self.var.as_mut()) {
            (Pacing::WaitForVsync, _) => wait_for_vsync(fd).err(),
            (Pacing::Pan, Some(var)) => pan_display(fd, var).err(),
            _ => None,
        };
        if let Some(errno) = failed.filter(|e| *e != libc::EINTR) {
            println!("couch-gui: pacing: {:?} failed with {}, sleeping from now on",
                     self.pacing, errno_name(errno));
            self.pacing = Pacing::Sleep;
        }
        // A wait that outlasts several frames is not pacing any more, it is
        // the loop being held by the kernel: input queues, animations stop,
        // and on this panel it lasted as long as a key was down. One line,
        // and never that ioctl again for the life of the process.
        let waited = before.elapsed();
        if self.pacing != Pacing::Sleep && waited > STALL {
            println!("couch-gui: pacing: {:?} took {}ms, sleeping from now on",
                     self.pacing, waited.as_millis());
            self.pacing = Pacing::Sleep;
        }
        if self.pacing == Pacing::Sleep {
            let left = (started + FRAME).saturating_duration_since(Instant::now());
            if !left.is_zero() {
                std::thread::sleep(left);
            }
        }
    }
}

/// The buffer as the framebuffer sees it. Abgr is `repr(transparent)` over
/// the u32 the panel takes, so this is a view, not a conversion.
fn pixels(buf: &[Abgr]) -> &[u32] {
    unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u32, buf.len()) }
}

/// Slint's `ease-out`, cubic-bezier(0, 0, 0.58, 1), so a copied slide moves
/// the way the rasterised one did and the way everything else here still
/// does. The curve is x(s), y(s) over a parameter; x is solved for by
/// bisection, which it can be because x is monotonic in s.
fn ease_out(t: f32) -> f32 {
    const X2: f32 = 0.58;
    let x = |s: f32| 3.0 * (1.0 - s) * s * s * X2 + s * s * s;
    let y = |s: f32| 3.0 * (1.0 - s) * s * s + s * s * s;
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        if x(mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    y(0.5 * (lo + hi))
}

/// Some(period) if the ioctl works and blocks; otherwise the reason it does
/// not is appended to `why`.
fn probe(name: &str, why: &mut Vec<String>, call: &mut dyn FnMut() -> Result<(), i32>) -> Option<Duration> {
    let mut took = Duration::ZERO;
    for _ in 0..2 {
        let t = Instant::now();
        if let Err(errno) = call() {
            why.push(format!("{name}: {}", errno_name(errno)));
            return None;
        }
        took = t.elapsed();
    }
    if took < Duration::from_millis(2) {
        why.push(format!("{name}: returned in {:.2}ms", took.as_secs_f64() * 1e3));
        return None;
    }
    Some(took)
}

fn wait_for_vsync(fd: libc::c_int) -> Result<(), i32> {
    let mut arg: u32 = 0;
    ioctl_result(unsafe { libc::ioctl(fd, FBIO_WAITFORVSYNC as _, &mut arg as *mut u32) })
}

fn pan_display(fd: libc::c_int, var: &mut [u32; 40]) -> Result<(), i32> {
    ioctl_result(unsafe { libc::ioctl(fd, FBIOPAN_DISPLAY as _, var.as_mut_ptr()) })
}

fn ioctl_result(rc: libc::c_int) -> Result<(), i32> {
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1))
    }
}

fn errno_name(errno: i32) -> String {
    match errno {
        libc::ENOTTY => "ENOTTY".into(),
        libc::EINVAL => "EINVAL".into(),
        libc::ENOSYS => "ENOSYS".into(),
        libc::EPERM => "EPERM".into(),
        libc::EINTR => "EINTR".into(),
        n => format!("errno {n}"),
    }
}

fn reasons(why: &[String]) -> String {
    if why.is_empty() {
        String::new()
    } else {
        format!(" ({})", why.join("; "))
    }
}

pub struct CouchPlatform {
    pub window: Rc<MinimalSoftwareWindow>,
    start: Instant,
}

impl CouchPlatform {
    /// ReusedBuffer, because we hand back the same RAM buffer every frame and
    /// it still holds the previous one - that is what enables partial redraw.
    pub fn install(size: PhysicalSize) -> Result<Rc<MinimalSoftwareWindow>, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        window.set_size(size);
        // set_platform reports its own error type, which is not PlatformError.
        slint::platform::set_platform(Box::new(CouchPlatform {
            window: window.clone(),
            start: Instant::now(),
        }))
        .map_err(|e| PlatformError::from(format!("set_platform: {e:?}")))?;
        Ok(window)
    }
}

impl Platform for CouchPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
}
