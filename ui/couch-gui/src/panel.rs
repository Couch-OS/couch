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

/// How long the iris takes to open or close.
///
/// One constant on purpose: this is the number to turn on the device. It is
/// deliberately slower than a page slide - the window travels much further
/// than a page does, and at the slide's 180 ms it reads as a flash rather
/// than as the row opening.
pub const IRIS: Duration = Duration::from_millis(300);

/// Where the focus ring sits relative to the window it rides, which is where
/// `FocusRing` sits relative to a row: `ring-offset` plus `ring-width`.
const IRIS_RING_BLEED: i32 = 6;
/// Its thickness, the same `Theme.ring-width` the row's own ring has.
const IRIS_RING_WIDTH: i32 = 3;
/// How much of the transition the ring rides for. It is not faded out - a
/// fade is a per-pixel blend, and this compositor never blends anything - so
/// it rides the window all the way: the window ends at the panel's edges and
/// the ring sits outside the window, so it leaves the panel by itself, edge
/// by edge, and is never seen to stop. A lower value cuts it off early, which
/// on a row near the top showed as the bottom of the ring vanishing while it
/// was still a hundred and fifty pixels from the edge.
const IRIS_RING_UNTIL: f32 = 1.0;
/// The ring's colour, packed the way the panel takes it. `Theme.accent` is
/// #FFFFFF, which is the same word whichever way round the channels go.
const IRIS_RING: u32 = 0xffff_ffff;

/// The lift's default time. It has more to say than a window opening - rows
/// falling away, a card travelling, cards arriving - and reads hurried at the
/// iris's 300 ms.
pub const LIFT: Duration = Duration::from_millis(320);

/// `Theme.bg`, #15130F, packed the way the panel takes it: what a room row
/// falls away to, and what the control screen's pieces arrive over.
const LIFT_BG: u32 = 0xff0f_1315;
/// `Theme.surface`, #1F1C17: a card's fill, which is both what the rising
/// card is drawn in and the colour keyed out of the sprites cut from a row.
const LIFT_SURFACE: u32 = 0xff17_1c1f;
/// `Theme.border`, #2C271F, for that card's edge.
const LIFT_BORDER: u32 = 0xff1f_272c;
/// The rising card's rectangle at the end of its travel, and its radius:
/// `Theme.r-card` at the header band, as the preview has it.
const LIFT_CARD_TO: (i32, i32, i32, i32, i32) = (20, 14, 440, 76, 12);
/// How far below their places the control screen's bar cards start, and how
/// much later the right-hand one arrives than the left.
const LIFT_BARS_DROP: i32 = 14;
const LIFT_BARS_LAG: f32 = 0.06;
/// How long one band of the room takes to fall away, as a fraction of the
/// whole. Short on purpose: it is the only blending in the transition, and
/// only the bands inside their own window are blended, so keeping it near
/// twice the stagger holds the blended part of the panel to about a quarter
/// of it however many rows there are.
const LIFT_BAND_FADE: f32 = 0.09;
/// How much later each band away from the focused row begins.
const LIFT_BAND_STEP: f32 = 0.045;
/// The phases, as fractions of the transition, named the way the preview
/// this follows names them (scratchpad/transitions, concept "lift").
const LIFT_ROWS_FALL: (f32, f32) = (0.02, 0.32);
const LIFT_CARD_RISE: (f32, f32) = (0.04, 0.56);
const LIFT_LABEL_FLY: (f32, f32) = (0.06, 0.58);
const LIFT_HEADER_IN: (f32, f32) = (0.50, 0.66);
const LIFT_CARDS_IN: (f32, f32) = (0.42, 0.82);
const LIFT_GROW: (f32, f32) = (0.62, 1.0);
const LIFT_FOOTER_IN: (f32, f32) = (0.80, 1.0);

/// Where the development switch for the opening transition is read from.
/// Under `/tmp`, so it is gone at the next boot and nothing a person set up is
/// ever changed by it.
const OPENING_SWITCH: &str = "/tmp/couch-transition";

/// The shape a control screen opens out of its row in.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Opening {
    /// The row's card grows to the panel on all four sides.
    Iris,
    /// The row's band, the whole width of the panel, opens up and down.
    Curtain,
    /// The room falls away from the row outwards, the row's card rises into
    /// the header, and the screen's cards arrive from below it.
    Lift,
}

/// How a control screen opens: the shape, and how long it takes.
///
/// Three shapes are in so they can be compared on the device, the one way to
/// judge a transition. `echo "curtain 220" > /tmp/couch-transition` on the
/// remote takes effect on the next press; no file is the iris at [`IRIS`].
/// The iris and the curtain are the same compositor with a different first
/// window and cost the same; the lift blends, and says what it cost in the
/// line it prints when it is over.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Transition {
    pub opening: Opening,
    pub time: Duration,
}

impl Default for Transition {
    fn default() -> Self {
        Transition {
            opening: Opening::Iris,
            time: IRIS,
        }
    }
}

impl Transition {
    /// What the switch file says, or the default. Read at each opening: it is
    /// a few bytes from a RAM disk, once per key press.
    pub fn chosen() -> Self {
        std::fs::read_to_string(OPENING_SWITCH)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }
    /// `iris`, `curtain` or `lift`, then optionally milliseconds. Anything
    /// that is not understood is the default for that part, and the time is
    /// kept between a tenth of a second and a whole one.
    fn parse(text: &str) -> Self {
        let mut chosen = Self::default();
        let mut given = None;
        for word in text.split_whitespace() {
            match word {
                "iris" => chosen.opening = Opening::Iris,
                "curtain" => chosen.opening = Opening::Curtain,
                "lift" => chosen.opening = Opening::Lift,
                other => {
                    if let Ok(ms) = other.parse::<u64>() {
                        given = Some(Duration::from_millis(ms.clamp(100, 1000)));
                    }
                }
            }
        }
        // Each shape has a time that suits it; a number in the file is what
        // the person on the remote wants instead, whichever shape it is.
        chosen.time = given.unwrap_or(match chosen.opening {
            Opening::Lift => LIFT,
            Opening::Iris | Opening::Curtain => IRIS,
        });
        chosen
    }
    /// The window this opening starts from (and closes onto) for a row.
    pub fn from_row(self, row: Window, width: u32) -> Window {
        match self.opening {
            // The lift never opens a window; the row is its card.
            Opening::Iris | Opening::Lift => row,
            Opening::Curtain => Window {
                x: 0,
                w: width as i32,
                ..row
            },
        }
    }
}

/// A rounded-rectangle window onto one page over another, in panel pixels.
///
/// Both ends of the travel are parameters, so the same compositor can open a
/// window from any rect to any other: a room row into the control screen
/// today, a device row into its packaged controls if that is wanted later.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub struct Window {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub r: i32,
}

impl Window {
    /// The whole panel, square-cornered: the far end of an opening iris.
    pub fn panel(width: u32, height: u32) -> Self {
        Window {
            x: 0,
            y: 0,
            w: width as i32,
            h: height as i32,
            r: 0,
        }
    }
    /// This window `t` of the way to `other`, every number an integer so the
    /// row loop below never touches a float.
    fn lerp(self, other: Window, t: f32) -> Window {
        let at = |a: i32, b: i32| a + (((b - a) as f32) * t).round() as i32;
        Window {
            x: at(self.x, other.x),
            y: at(self.y, other.y),
            w: at(self.w, other.w),
            h: at(self.h, other.h),
            r: at(self.r, other.r),
        }
    }
    /// The same window grown by `n` pixels on every side, corners included:
    /// the rect the ring rides.
    fn grown(self, n: i32) -> Window {
        Window {
            x: self.x - n,
            y: self.y - n,
            w: self.w + 2 * n,
            h: self.h + 2 * n,
            r: self.r + n,
        }
    }
    /// And shrunk, which is how a stroke is drawn: the outline is the outer
    /// window minus this one.
    fn shrunk(self, n: i32) -> Window {
        Window {
            x: self.x + n,
            y: self.y + n,
            w: self.w - 2 * n,
            h: self.h - 2 * n,
            r: (self.r - n).max(0),
        }
    }
}

/// Which page an iris shows inside its window; the other one surrounds it.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Shown {
    /// The page being arrived at: a control screen opening out of its row.
    Arriving,
    /// The page the panel is already showing: that screen closing back onto
    /// the row, with the room behind it already rendered.
    Leaving,
}

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
        let rc =
            unsafe { libc::ioctl(fb.as_raw_fd(), FBIOGET_VSCREENINFO as _, vinfo.as_mut_ptr()) };
        let var = (rc == 0 && vinfo[0] > 0 && vinfo[1] > 0).then_some(vinfo);
        let (width, height) = var.map_or((480, 800), |v| (v[0], v[1]));
        let stride = std::fs::read_to_string("/sys/class/graphics/fb0/stride")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(width * 4);

        let len = (stride * height) as usize;
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fb.as_raw_fd(),
                0,
            )
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
            var: var.map(|mut v| {
                v[4] = 0;
                v[5] = 0;
                v
            }),
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
                println!(
                    "couch-gui: pacing: FBIO_WAITFORVSYNC, {:.1}ms per wait{}",
                    period.as_secs_f64() * 1e3,
                    reasons(&why)
                );
                return Pacing::WaitForVsync;
            }
        }
        if try_pan {
            match self.var {
                Some(mut var) => {
                    if let Some(period) = probe("FBIOPAN_DISPLAY", &mut why, &mut || {
                        pan_display(fd, &mut var)
                    }) {
                        println!(
                            "couch-gui: pacing: FBIOPAN_DISPLAY, {:.1}ms per wait{}",
                            period.as_secs_f64() * 1e3,
                            reasons(&why)
                        );
                        return Pacing::Pan;
                    }
                }
                None => why.push("FBIOPAN_DISPLAY: no screeninfo".into()),
            }
        }
        println!(
            "couch-gui: pacing: sleep to {:.2}ms{}",
            FRAME.as_secs_f64() * 1e3,
            reasons(&why)
        );
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
        let arg = if off {
            FB_BLANK_POWERDOWN
        } else {
            FB_BLANK_UNBLANK
        };
        let rc = unsafe { libc::ioctl(self.fb.as_raw_fd(), FBIOBLANK as _, arg as libc::c_ulong) };
        if rc != 0 {
            println!(
                "couch-gui: FBIOBLANK({arg}) failed: {}",
                std::io::Error::last_os_error()
            );
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
                println!(
                    "couch-gui: present: FBIOPAN_DISPLAY failed with {}",
                    errno_name(e)
                );
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
        let (w, h, stride) = (
            self.width as usize,
            self.height as usize,
            self.stride_px as usize,
        );
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
        self.draw(window, false)
            .then(|| started.elapsed().as_micros() as u64)
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
                        " [{},{} {}x{}]",
                        pos.x, pos.y, sz.width, sz.height
                    ));
                }
                println!(
                    "couch-gui: region {n} rect(s), {px} px = {}% of screen{where_}",
                    px * 100 / (w as u64 * h as u64)
                );
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
                println!(
                    "couch-gui: slide frame {}: dx {dx}, {work_us} us, {wait_us} us paced",
                    cost.frames
                );
            }
            if t >= 1.0 {
                return cost;
            }
        }
    }

    /// Open a rounded window from `from` to `to`, on the panel, without
    /// rendering anything.
    ///
    /// Same two buffers as `slide` and the same pacing: `snapshot` with the
    /// old page showing, change the state, `render_offscreen` the new one.
    /// Where a slide moves one page off the side, this cuts a growing
    /// rounded rect out of one page and shows the other through it, which is
    /// three `copy_from_slice` runs a row instead of two - A, B, A - and one
    /// square root on each of the `2r` rows at the two ends. Nothing is
    /// blended and nothing is re-rasterised, so a frame costs what a slide
    /// frame costs: about a millisecond of copying, whatever the clock.
    ///
    /// The last frame is the whole of the page that is arriving, so when this
    /// returns the panel and RAM agree again - including on a close, where
    /// the window stops at the row rather than at nothing.
    pub fn iris(
        &mut self,
        from: Window,
        to: Window,
        shown: Shown,
        duration: Duration,
    ) -> SlideCost {
        self.transition(Compose::Iris { from, to }, shown, duration)
    }

    /// The lift: the room falls away from the focused row outwards, that
    /// row's card rises into the header band and hands over, and the control
    /// screen's bar cards arrive from a little below their places.
    ///
    /// Composed from the same two buffers as the iris, with two primitives
    /// the iris does not need: a band of one page copied to a different `y`,
    /// and a per-pixel cross-fade. It is the one transition here that blends,
    /// so it is the one whose cost has to be read rather than assumed - the
    /// line the loop prints when it is over says what it was.
    pub fn lift(&mut self, lift: Lift, shown: Shown, duration: Duration) -> SlideCost {
        let (w, h) = (self.width as usize, self.height as usize);
        // The room is where the name and the icon are cut from, whichever way
        // the transition is going. Cut once, here, before a frame is composed.
        let label;
        let disc;
        {
            let room = match shown {
                Shown::Arriving => pixels(&self.spare),
                Shown::Leaving => pixels(&self.ram),
            };
            label = Sprite::cut(room, w, h, lift.label);
            disc = Sprite::cut(room, w, h, lift.disc);
        }
        self.transition(
            Compose::Lift {
                lift,
                sprites: (&label, &disc),
            },
            shown,
            duration,
        )
    }

    /// The frame loop every opening shares: eased time, one composed frame,
    /// the same pacing a drawn frame gets, and the same per-frame report, so
    /// two shapes can be compared on one set of numbers.
    fn transition(&mut self, what: Compose<'_>, shown: Shown, duration: Duration) -> SlideCost {
        let report = std::env::var_os("COUCH_REGION").is_some();
        let duration = duration.max(FRAME).as_secs_f32();
        let began = Instant::now();
        let mut cost = SlideCost::default();
        loop {
            let started = Instant::now();
            // As in `slide`: a frame composed now reaches the glass at the
            // next refresh, so it is drawn one period ahead of the clock.
            let t = ((started.duration_since(began) + FRAME).as_secs_f32() / duration).min(1.0);
            let done = t >= 1.0;
            match what {
                Compose::Iris { from, to } => self.compose_iris(from, to, shown, t),
                Compose::Lift { lift, sprites } => self.compose_lift(lift, sprites, shown, t),
            }
            let work = started.elapsed();
            self.pace(started);
            let wait = started.elapsed().saturating_sub(work);
            let (work_us, wait_us) = (work.as_micros() as u64, wait.as_micros() as u64);
            cost.frames += 1;
            cost.work_us += work_us;
            cost.wait_us += wait_us;
            cost.max_us = cost.max_us.max(work_us);
            if report {
                match what {
                    Compose::Iris { from, to } => {
                        let window = from.lerp(to, ease_out(t));
                        println!(
                            "couch-gui: iris frame {}: {}x{} at {},{} r{}, {work_us} us, {wait_us} us paced",
                            cost.frames, window.w, window.h, window.x, window.y, window.r
                        );
                    }
                    Compose::Lift { .. } => println!(
                        "couch-gui: lift frame {}: t {:.2}, {work_us} us, {wait_us} us paced",
                        cost.frames, t
                    ),
                }
            }
            if done {
                return cost;
            }
        }
    }

    /// One lift frame into the framebuffer.
    fn compose_lift(&mut self, lift: Lift, sprites: (&Sprite, &Sprite), shown: Shown, t: f32) {
        let (width, height, stride) = (
            self.width as usize,
            self.height as usize,
            self.stride_px as usize,
        );
        let (arriving, leaving) = (pixels(&self.ram), pixels(&self.spare));
        lift_frame(
            Surface {
                pixels: &mut self.map[..],
                stride,
                width,
                height,
            },
            (arriving, leaving),
            lift,
            sprites,
            shown,
            t,
        );
    }

    /// One iris frame into the framebuffer.
    fn compose_iris(&mut self, from: Window, to: Window, shown: Shown, t: f32) {
        let (width, height, stride) = (
            self.width as usize,
            self.height as usize,
            self.stride_px as usize,
        );
        let (arriving, leaving) = (pixels(&self.ram), pixels(&self.spare));
        iris_frame(
            Surface {
                pixels: &mut self.map[..],
                stride,
                width,
                height,
            },
            (arriving, leaving),
            (from, to),
            shown,
            t,
        );
    }

    /// One transition frame straight into the framebuffer: A shifted `dx`
    /// pixels out of the way and B filling what it uncovered, except the
    /// `keep` bands, which are B as they stand.
    fn compose(&mut self, from: Arrive, dx: usize, keep: &[(u32, u32)]) {
        let (w, h, stride) = (
            self.width as usize,
            self.height as usize,
            self.stride_px as usize,
        );
        let dx = dx.min(w);
        let (a, b) = (pixels(&self.spare), pixels(&self.ram));
        for y in 0..h {
            let dst = &mut self.map[y * stride..y * stride + w];
            let (ra, rb) = (&a[y * w..(y + 1) * w], &b[y * w..(y + 1) * w]);
            let held = keep
                .iter()
                .any(|&(top, height)| y >= top as usize && y < (top + height) as usize);
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
            println!(
                "couch-gui: pacing: {:?} failed with {}, sleeping from now on",
                self.pacing,
                errno_name(errno)
            );
            self.pacing = Pacing::Sleep;
        }
        // A wait that outlasts several frames is not pacing any more, it is
        // the loop being held by the kernel: input queues, animations stop,
        // and on this panel it lasted as long as a key was down. One line,
        // and never that ioctl again for the life of the process.
        let waited = before.elapsed();
        if self.pacing != Pacing::Sleep && waited > STALL {
            println!(
                "couch-gui: pacing: {:?} took {}ms, sleeping from now on",
                self.pacing,
                waited.as_millis()
            );
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

/// Somewhere to compose into: the panel's own map, or a plain vector in a
/// test. The stride is separate from the width because the framebuffer's rows
/// are wider than the panel on some of these devices.
pub(crate) struct Surface<'a> {
    pub pixels: &'a mut [u32],
    pub stride: usize,
    pub width: usize,
    pub height: usize,
}

/// What an iris puts on the panel `t` of the way through: the window eased to
/// where it should be, one page seen through it and the other around it, and
/// the focus ring on its edge while it still has one.
///
/// A free function over a `Surface` rather than a method, so the pictures the
/// tests keep come out of exactly the code the panel runs.
pub(crate) fn iris_frame(
    mut dst: Surface<'_>,
    pages: (&[u32], &[u32]),
    travel: (Window, Window),
    shown: Shown,
    t: f32,
) {
    let (arriving, leaving) = pages;
    let last = t >= 1.0;
    // Which page is inside the window depends on the direction. On the last
    // frame neither is: whatever the window was doing, the page that is
    // arriving is what is left on the panel, so RAM and the panel agree from
    // here on - including on a close, where the window stops at the row.
    let (outside, inside) = match (last, shown) {
        (true, _) => (arriving, arriving),
        (false, Shown::Arriving) => (leaving, arriving),
        (false, Shown::Leaving) => (arriving, leaving),
    };
    let window = if last {
        Window::default()
    } else {
        travel.0.lerp(travel.1, ease_out(t))
    };
    compose_window(&mut dst, outside, inside, window);
    if !last && t < IRIS_RING_UNTIL {
        stroke_window(
            &mut dst,
            window.grown(IRIS_RING_BLEED),
            IRIS_RING_WIDTH,
            IRIS_RING,
        );
    }
}

/// Which shape a transition is composing, so that one frame loop can time,
/// pace and report for all of them on the same terms.
#[derive(Copy, Clone)]
enum Compose<'a> {
    Iris {
        from: Window,
        to: Window,
    },
    Lift {
        lift: Lift,
        sprites: (&'a Sprite, &'a Sprite),
    },
}

/// Everything the lift needs to know about the two pages, in panel pixels.
///
/// All of it is read from the pages themselves - the list for the room's
/// half, `light.slint` for the screen's - once, before the first frame.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Lift {
    /// The row the screen is opening out of, and one row to the next.
    pub row: Window,
    pub pitch: i32,
    /// Where that row's name and icon are drawn inside it.
    pub label: Window,
    pub disc: Window,
    /// Where the two of them land on the screen.
    pub title: Window,
    pub screen_disc: Window,
    /// The bands of the screen that arrive rather than being there: the
    /// header down to the state line, the bar cards, and the footer.
    pub header_h: i32,
    pub cards: [Window; 2],
    pub footer_y: i32,
    /// The level bar's track and how full it ends up, and where the colour
    /// marker ends up, so both can be revealed out of the screen.
    pub track: [Window; 2],
    pub fill_h: i32,
    pub marker_y: i32,
    pub marker_h: i32,
}

/// A rectangle cut out of a page, kept for the length of one transition.
///
/// Small and owned: a row's name and its icon, a few tens of kilobytes
/// between them, taken once before the first frame and put down at an
/// interpolated place on each. Nothing here allocates once the transition has
/// started.
pub(crate) struct Sprite {
    pixels: Vec<u32>,
    box_: Window,
}

impl Sprite {
    /// Cut a rectangle out of a page.
    pub(crate) fn cut(page: &[u32], width: usize, height: usize, box_: Window) -> Sprite {
        let (x, y) = (box_.x.max(0) as usize, box_.y.max(0) as usize);
        let w = (box_.w.max(0) as usize).min(width.saturating_sub(x));
        let h = (box_.h.max(0) as usize).min(height.saturating_sub(y));
        let mut pixels = Vec::with_capacity(w * h);
        for row in 0..h {
            pixels.extend_from_slice(&page[(y + row) * width + x..(y + row) * width + x + w]);
        }
        Sprite {
            pixels,
            box_: Window {
                x: x as i32,
                y: y as i32,
                w: w as i32,
                h: h as i32,
                r: 0,
            },
        }
    }
    /// Put it down with its top-left corner at `at`, clipped at all four
    /// edges of the surface. `key` is a colour that is not drawn: the card a
    /// row's name sits on, so that only the name travels and not the plate
    /// under it. Written, never read back - a framebuffer is slow to read.
    pub(crate) fn put(&self, dst: &mut Surface<'_>, at: (i32, i32), key: Option<u32>) {
        let (w, h, stride) = (dst.width as i32, dst.height as i32, dst.stride);
        for row in 0..self.box_.h {
            let y = at.1 + row;
            if y < 0 || y >= h {
                continue;
            }
            let (from, to) = (row * self.box_.w, at.0);
            let left = to.max(0);
            let right = (to + self.box_.w).min(w);
            if left >= right {
                continue;
            }
            let taken = &self.pixels[(from + left - to) as usize..(from + right - to) as usize];
            let put = &mut dst.pixels
                [y as usize * stride + left as usize..y as usize * stride + right as usize];
            match key {
                None => put.copy_from_slice(taken),
                Some(key) => {
                    for (d, &s) in put.iter_mut().zip(taken) {
                        if s != key {
                            *d = s;
                        }
                    }
                }
            }
        }
    }
}

/// A filled rounded rectangle with a one-pixel edge, in the colours a card is
/// drawn in: the row's card, rising.
///
/// The same row-run arithmetic the focus ring's outline uses, filled instead
/// of stroked, so it costs a run a scanline over a rectangle that is never
/// more than a tenth of the panel.
fn fill_window(dst: &mut Surface<'_>, box_: Window, fill: u32, edge: u32) {
    let (w, h, stride) = (dst.width, dst.height, dst.stride);
    for y in 0..h {
        let Some((left, right)) = window_run(box_, y as i32, w) else {
            continue;
        };
        let row = &mut dst.pixels[y * stride + left..y * stride + right];
        row.fill(fill);
        // The edge: the first and last row of the box, and a pixel each side
        // of every row, which follows the corner inset by construction.
        let ends = y as i32 == box_.y || y as i32 == box_.y + box_.h - 1;
        if ends {
            row.fill(edge);
        } else {
            if let Some(first) = row.first_mut() {
                *first = edge;
            }
            if let Some(last) = row.last_mut() {
                *last = edge;
            }
        }
    }
}

/// One lift frame: the room falling away from the focused row outwards, that
/// row's card rising into the header band with its name and icon riding on
/// it, and the control screen arriving piece by piece.
///
/// The close is the open run backwards over the same two pages, so `t` of 0
/// is always the room and `t` of 1 always the screen whichever way it is
/// going - which is what keeps the last frame of either a whole page.
pub(crate) fn lift_frame(
    mut dst: Surface<'_>,
    pages: (&[u32], &[u32]),
    lift: Lift,
    sprites: (&Sprite, &Sprite),
    shown: Shown,
    t: f32,
) {
    let (arriving, leaving) = pages;
    let (room, screen, p) = match shown {
        Shown::Arriving => (leaving, arriving, t),
        Shown::Leaving => (arriving, leaving, 1.0 - t),
    };
    let (w, h, stride) = (dst.width, dst.height, dst.stride);
    // The end is the page itself, whole: every piece has arrived and nothing
    // is left to work out.
    if p >= 1.0 {
        for y in 0..h {
            dst.pixels[y * stride..y * stride + w].copy_from_slice(&screen[y * w..(y + 1) * w]);
        }
        return;
    }
    // --- the room falls away, band by band, outwards from the row ---------
    // A band is one row's pitch, so a card is never caught half faded. Only
    // the bands inside their own short window are blended; before it a band
    // is a copy of the room and after it a fill, which is what the screen's
    // background is anyway.
    let (fall_a, fall_b) = LIFT_ROWS_FALL;
    for y in 0..h {
        let band = (y as i32 - lift.row.y).div_euclid(lift.pitch.max(1));
        let began = fall_a + LIFT_BAND_STEP * band.unsigned_abs() as f32;
        let out = &mut dst.pixels[y * stride..y * stride + w];
        if p < began {
            out.copy_from_slice(&room[y * w..(y + 1) * w]);
        } else if p < (began + LIFT_BAND_FADE).min(fall_b) {
            let k = 256 - (((p - began) / LIFT_BAND_FADE).min(1.0) * 256.0).round() as u32;
            blend_flat(out, &room[y * w..(y + 1) * w], LIFT_BG, k);
        } else {
            out.fill(LIFT_BG);
        }
    }
    // --- the focused card rises into the header band ----------------------
    if let Some(e) = phase(p, LIFT_CARD_RISE) {
        let (x, y, cw, ch, r) = LIFT_CARD_TO;
        let to = Window {
            x,
            y,
            w: cw,
            h: ch,
            r,
        };
        fill_window(&mut dst, lift.row.lerp(to, e), LIFT_SURFACE, LIFT_BORDER);
    }
    // --- its name and its icon fly to the title and the disc --------------
    // Keyed on the card they were cut from, so only the glyphs travel.
    if let Some(e) = phase(p, LIFT_LABEL_FLY) {
        let (label, disc) = sprites;
        let at = |from: Window, to: Window| {
            (
                from.x + (((to.x - from.x) as f32) * e).round() as i32,
                from.y + (((to.y - from.y) as f32) * e).round() as i32,
            )
        };
        disc.put(
            &mut dst,
            at(lift.disc, lift.screen_disc),
            Some(LIFT_SURFACE),
        );
        label.put(
            &mut dst,
            at(lift.label, title_landing(lift)),
            Some(LIFT_SURFACE),
        );
    }
    // --- the real header takes over ---------------------------------------
    if p >= LIFT_HEADER_IN.0 {
        band_over(
            &mut dst,
            screen,
            Window {
                x: 0,
                y: 0,
                w: w as i32,
                h: lift.header_h,
                r: 0,
            },
            0,
            256,
        );
    }
    // --- the bar cards arrive, fourteen pixels low, the right one lagging -
    for (which, card) in lift.cards.iter().enumerate() {
        if card.w <= 0 {
            continue;
        }
        let window = (
            LIFT_CARDS_IN.0 + LIFT_BARS_LAG * which as f32,
            LIFT_CARDS_IN.1 + LIFT_BARS_LAG * which as f32,
        );
        let Some(e) = phase(p, window) else {
            continue;
        };
        let dy = (LIFT_BARS_DROP as f32 * (1.0 - e)).round() as i32;
        band_over(&mut dst, screen, *card, dy, 256);
        // The level and the colour it is showing grow into the card rather
        // than arriving with it: the screen already holds both at their
        // values, so they are revealed out of it.
        reveal_track(&mut dst, screen, lift, which, dy, p);
    }
    // --- the footer, last ---------------------------------------------------
    if p >= LIFT_FOOTER_IN.0 {
        band_over(
            &mut dst,
            screen,
            Window {
                x: 0,
                y: lift.footer_y,
                w: w as i32,
                h: h as i32 - lift.footer_y,
                r: 0,
            },
            0,
            256,
        );
    }
}

/// Where the flying label comes to rest: the title's box, lined up so that
/// the name lands on the name rather than the box on the box - the two are
/// the same size and weight (`Theme.device-name`), so this is a cut, not a
/// fade.
fn title_landing(lift: Lift) -> Window {
    Window {
        x: lift.title.x,
        y: lift.title.y + (lift.title.h - lift.label.h) / 2,
        ..lift.label
    }
}

/// How far through a phase `p` is, or nothing because it has not begun.
/// Eased the way everything else here is once it has.
fn phase(p: f32, window: (f32, f32)) -> Option<f32> {
    (p >= window.0).then(|| {
        let span = (window.1 - window.0).max(f32::EPSILON);
        ease_out(((p - window.0) / span).clamp(0.0, 1.0))
    })
}

/// The level bar filling and the colour marker sliding to their places.
///
/// Neither is drawn: the page already holds both at their values, and the
/// card's own band copy has just put them down. The fill is un-revealed from
/// the top by painting the empty part of the track in the colour the page
/// gives it, and the marker is moved by painting over it with the gradient
/// from just above and putting it back where it has got to. While the card is
/// still travelling, both move with it.
fn reveal_track(dst: &mut Surface<'_>, screen: &[u32], lift: Lift, which: usize, dy: i32, p: f32) {
    let track = lift.track[which];
    if track.w <= 0 {
        return;
    }
    let grown = phase(p, LIFT_GROW).unwrap_or(0.0);
    let width = dst.width;
    if which == 0 && lift.fill_h > 0 {
        let risen = (lift.fill_h as f32 * grown).round() as i32;
        let empty = track.y + track.h - risen;
        // The colour of an empty track, from the page's own top of it.
        let sample = screen
            .get(((track.y + 2).max(0) as usize) * width + (track.x + track.w / 2).max(0) as usize)
            .copied()
            .unwrap_or(LIFT_BG);
        let (left, right) = (
            track.x.clamp(0, width as i32) as usize,
            (track.x + track.w).clamp(0, width as i32) as usize,
        );
        for y in track.y..empty {
            let row = y + dy;
            if row < 0 || row >= dst.height as i32 || left >= right {
                continue;
            }
            dst.pixels[row as usize * dst.stride + left..row as usize * dst.stride + right]
                .fill(sample);
        }
    } else if which == 1 && lift.marker_y >= 0 {
        let strip = Window {
            x: track.x,
            y: lift.marker_y,
            w: track.w,
            h: lift.marker_h,
            r: 0,
        };
        let foot = track.y + track.h - lift.marker_h;
        let at = foot + ((lift.marker_y - foot) as f32 * grown).round() as i32;
        if at != lift.marker_y {
            // Where the page keeps it, painted out with the gradient just
            // above it - which is the same gradient, a few mirek along.
            band_over(
                dst,
                screen,
                Window {
                    y: lift.marker_y - lift.marker_h,
                    ..strip
                },
                lift.marker_h + dy,
                256,
            );
        }
        band_over(dst, screen, strip, at - lift.marker_y + dy, 256);
    }
}

/// A band of `src`, `dy` pixels from where it sits there, put into the
/// surface: `k` of 256 of it, or copied outright when that is all of it.
///
/// One `copy_from_slice` or one blended run a scanline, clipped at all four
/// edges, nothing allocated. The cheap half of what the lift needs.
fn band_over(dst: &mut Surface<'_>, src: &[u32], band: Window, dy: i32, k: u32) {
    let (w, h, stride) = (dst.width, dst.height, dst.stride);
    let left = band.x.clamp(0, w as i32) as usize;
    let right = (band.x + band.w).clamp(left as i32, w as i32) as usize;
    if left == right || k == 0 {
        return;
    }
    for row in 0..band.h.max(0) {
        let (from, to) = (band.y + row, band.y + row + dy);
        if from < 0 || from >= h as i32 || to < 0 || to >= h as i32 {
            continue;
        }
        let (from, to) = (from as usize, to as usize);
        let taken = &src[from * w + left..from * w + right];
        let put = &mut dst.pixels[to * stride + left..to * stride + right];
        if k >= 256 {
            put.copy_from_slice(taken);
        } else {
            blend_over(put, taken, k);
        }
    }
}

/// One scanline of `src` crossed to a flat colour: `k` of 256 is how much of
/// `src`, so 256 is the page untouched and 0 is the colour.
///
/// Two pixels to an iteration and two channels to a multiply: four channels
/// packed in the spare halves of a `u64`, red and blue of both pixels in one
/// pass and green and alpha in the other. Each product is at most `255 * 256`,
/// which is exactly sixteen bits, so no lane can carry into its neighbour and
/// nothing has to be unpacked. The flat operand's two halves are worked out
/// once for the whole run.
///
/// The u32-at-a-time version of this measured about 10 ms for a whole panel
/// on the HA100 against 1.3 ms for a `memcpy` of the same, which is far more
/// than a dozen integer operations a pixel should cost: it was not being
/// widened. Halving the iterations is the part of that worth having without
/// reaching for intrinsics.
fn blend_flat(dst: &mut [u32], src: &[u32], flat: u32, k: u32) {
    debug_assert!(k <= 256);
    const LO: u64 = 0x00ff_00ff_00ff_00ff;
    const HI: u64 = 0xff00_ff00_ff00_ff00;
    let (ks, kf) = (u64::from(k), u64::from(256 - k));
    let both = (u64::from(flat) << 32) | u64::from(flat);
    let (flat_lo, flat_hi) = ((both & LO) * kf, ((both >> 8) & LO) * kf);
    let n = dst.len().min(src.len());
    let (pairs, tail) = dst[..n].as_chunks_mut::<2>();
    let (src_pairs, src_tail) = src[..n].as_chunks::<2>();
    for (d, s) in pairs.iter_mut().zip(src_pairs) {
        let packed = (u64::from(s[1]) << 32) | u64::from(s[0]);
        let lo = ((packed & LO) * ks + flat_lo) >> 8;
        let hi = (((packed >> 8) & LO) * ks + flat_hi) & HI;
        let out = (lo & LO) | hi;
        d[0] = out as u32;
        d[1] = (out >> 32) as u32;
    }
    for (d, &s) in tail.iter_mut().zip(src_tail) {
        let lo = ((u64::from(s) & LO) * ks + flat_lo) >> 8;
        let hi = (((u64::from(s) >> 8) & LO) * ks + flat_hi) & HI;
        *d = ((lo & LO) | hi) as u32;
    }
}

/// The same crossing, in place: `k` of 256 of `src` into what is already in
/// `dst`. What the travelling card is laid over the frame with.
fn blend_over(dst: &mut [u32], src: &[u32], k: u32) {
    debug_assert!(k <= 256);
    let (kd, ks) = (256 - k, k);
    for (d, &s) in dst.iter_mut().zip(src) {
        let p = *d;
        let lo = ((p & 0x00ff_00ff) * kd + (s & 0x00ff_00ff) * ks) >> 8;
        let hi = (((p >> 8) & 0x00ff_00ff) * kd + ((s >> 8) & 0x00ff_00ff) * ks) & 0xff00_ff00;
        *d = (lo & 0x00ff_00ff) | hi;
    }
}

/// `inside` seen through the window and `outside` around it: three copies a
/// row and never a blend.
fn compose_window(dst: &mut Surface<'_>, outside: &[u32], inside: &[u32], window: Window) {
    let (w, h, stride) = (dst.width, dst.height, dst.stride);
    for y in 0..h {
        let row = &mut dst.pixels[y * stride..y * stride + w];
        let (ro, ri) = (&outside[y * w..(y + 1) * w], &inside[y * w..(y + 1) * w]);
        match window_run(window, y as i32, w) {
            Some((l, r)) => {
                row[..l].copy_from_slice(&ro[..l]);
                row[l..r].copy_from_slice(&ri[l..r]);
                row[r..].copy_from_slice(&ro[r..]);
            }
            None => row.copy_from_slice(ro),
        }
    }
}

/// The outline of a rounded window, `thickness` pixels thick, filled in one
/// colour: the outer window minus the same window that much smaller. Filled
/// rather than blended, like everything else here.
fn stroke_window(dst: &mut Surface<'_>, outer: Window, thickness: i32, colour: u32) {
    let (w, h, stride) = (dst.width, dst.height, dst.stride);
    let inner = outer.shrunk(thickness);
    for y in 0..h {
        let Some((lo, ro)) = window_run(outer, y as i32, w) else {
            continue;
        };
        let row = &mut dst.pixels[y * stride..y * stride + w];
        match window_run(inner, y as i32, w) {
            Some((li, ri)) => {
                row[lo..li].fill(colour);
                row[ri..ro].fill(colour);
            }
            None => row[lo..ro].fill(colour),
        }
    }
}

/// The run of the inner page on one row of a rounded window: nothing above or
/// below it, otherwise `[left, right)` clamped to the panel, inset at the
/// corners.
///
/// Integer throughout and allocation-free, because it runs 800 times a frame.
/// Only the `2r` rows at the two ends pay for a square root, over a radius of
/// a dozen-odd pixels; every other row is two comparisons.
fn window_run(window: Window, y: i32, width: usize) -> Option<(usize, usize)> {
    if window.w <= 0 || window.h <= 0 || y < window.y || y >= window.y + window.h {
        return None;
    }
    let r = window.r.clamp(0, window.w.min(window.h) / 2);
    // Doubled coordinates, so the row's centre line stays an integer: how far
    // this row is past the centre of the corner circles, times two.
    let above = 2 * (window.y + r) - (2 * y + 1);
    let below = (2 * y + 1) - 2 * (window.y + window.h - r);
    let over = above.max(below);
    // The half-chord of the corner circle at this row, in quarter pixels, so
    // that flooring the square root costs a quarter of a pixel rather than
    // most of one; the inset is then rounded to the nearest whole pixel.
    let inset = if r > 0 && over > 0 {
        let half = isqrt((16 * r * r - 4 * over * over).max(0) as u32) as i32;
        (4 * r - half + 2) / 4
    } else {
        0
    };
    let left = (window.x + inset).clamp(0, width as i32) as usize;
    let right = (window.x + window.w - inset).clamp(left as i32, width as i32) as usize;
    (left < right).then_some((left, right))
}

/// Integer square root for the corner inset. The argument is at most `16r^2`
/// for a radius of a dozen pixels, so this is a few dozen comparisons on a
/// couple of dozen rows, and it keeps floating point out of the compositor.
fn isqrt(n: u32) -> u32 {
    let mut root = 0;
    while (root + 1) * (root + 1) <= n {
        root += 1;
    }
    root
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
fn probe(
    name: &str,
    why: &mut Vec<String>,
    call: &mut dyn FnMut() -> Result<(), i32>,
) -> Option<Duration> {
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
        why.push(format!(
            "{name}: returned in {:.2}ms",
            took.as_secs_f64() * 1e3
        ));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_opening_is_the_iris_unless_the_switch_says_otherwise() {
        assert_eq!(Transition::parse(""), Transition::default());
        assert_eq!(Transition::default().opening, Opening::Iris);
        assert_eq!(Transition::default().time, IRIS);
        let curtain = Transition::parse("curtain 220\n");
        assert_eq!(curtain.opening, Opening::Curtain);
        assert_eq!(curtain.time, Duration::from_millis(220));
        // Either part alone, in either order, and nonsense changes nothing.
        assert_eq!(Transition::parse("curtain").time, IRIS);
        assert_eq!(
            Transition::parse("400 iris").time,
            Duration::from_millis(400)
        );
        assert_eq!(Transition::parse("sideways fast"), Transition::default());
        // A time nobody could want is brought back into a range somebody could.
        assert_eq!(Transition::parse("5").time, Duration::from_millis(100));
        assert_eq!(Transition::parse("99999").time, Duration::from_millis(1000));
        // The lift keeps its own default, and takes a time like the others.
        let lift = Transition::parse("lift");
        assert_eq!(lift.opening, Opening::Lift);
        assert_eq!(lift.time, LIFT);
        assert_eq!(
            Transition::parse("lift 400").time,
            Duration::from_millis(400)
        );
        assert_eq!(Transition::parse("lift 400").opening, Opening::Lift);
        // It opens out of the row's own card, as the iris does.
        let row = Window {
            x: 20,
            y: 199,
            w: 440,
            h: 90,
            r: 14,
        };
        assert_eq!(lift.from_row(row, 480), row);
    }

    /// The two primitives the lift adds to the compositor, on buffers a few
    /// pixels across: a band of one page put down somewhere else, and one
    /// scanline of a page crossed to another.
    #[test]
    fn a_band_lands_where_it_is_put_and_a_blend_is_the_mean_of_its_ends() {
        // A four-pixel-wide band, two rows, moved up one row.
        let src: Vec<u32> = (0..W * H).map(|i| i as u32).collect();
        let mut pixels = vec![0u32; W * H];
        let band = Window {
            x: 2,
            y: 5,
            w: 4,
            h: 2,
            r: 0,
        };
        fn surface(pixels: &mut [u32]) -> Surface<'_> {
            Surface {
                pixels,
                stride: W,
                width: W,
                height: H,
            }
        }
        band_over(&mut surface(&mut pixels), &src, band, -1, 256);
        assert_eq!(&pixels[4 * W + 2..4 * W + 6], &src[5 * W + 2..5 * W + 6]);
        assert_eq!(&pixels[5 * W + 2..5 * W + 6], &src[6 * W + 2..6 * W + 6]);
        // Nothing either side of the band's columns, or on the rows it left.
        assert_eq!(pixels[4 * W + 1], 0);
        assert_eq!(pixels[4 * W + 6], 0);
        assert!(pixels[6 * W..7 * W].iter().all(|p| *p == 0));

        // Off the top: the row that would land outside is dropped and the
        // one that still fits arrives, with nothing wrapped round to the
        // other end of the panel.
        let mut pixels = vec![0u32; W * H];
        band_over(&mut surface(&mut pixels), &src, band, -6, 256);
        assert_eq!(&pixels[2..6], &src[6 * W + 2..6 * W + 6]);
        assert!(pixels[6..W].iter().all(|p| *p == 0));
        assert!(pixels[W..].iter().all(|p| *p == 0));
        // And a band wider than the panel is cut to it rather than wrapping.
        let mut pixels = vec![0u32; W * H];
        let wide = Window {
            x: -4,
            y: 5,
            w: 40,
            h: 1,
            r: 0,
        };
        band_over(&mut surface(&mut pixels), &src, wide, 0, 256);
        assert_eq!(&pixels[5 * W..6 * W], &src[5 * W..6 * W]);
        assert!(pixels[4 * W..5 * W].iter().all(|p| *p == 0));
        // Nothing of it at all when there is none of it to put down.
        let mut pixels = vec![7u32; W * H];
        band_over(&mut surface(&mut pixels), &src, band, -1, 0);
        assert!(pixels.iter().all(|p| *p == 7));

        // The crossing to a flat colour: all of the page, none of it, and the
        // mean of the two in between, channel by channel, alpha left alone.
        let page = [0xff20_4060u32, 0xffff_ffff];
        let flat = 0xffa0_c0e0u32;
        let mut out = [0u32; 2];
        blend_flat(&mut out, &page, flat, 256);
        assert_eq!(out, page);
        blend_flat(&mut out, &page, flat, 0);
        assert_eq!(out, [flat, flat]);
        blend_flat(&mut out, &page, flat, 128);
        assert_eq!(out, [0xff60_80a0, 0xffcf_dfef]);
        // And in place, over what is already there, is the same arithmetic
        // with two pages instead of a page and a colour.
        let mut over = page;
        blend_over(&mut over, &[flat, flat], 128);
        assert_eq!(over, [0xff60_80a0, 0xffcf_dfef]);
        blend_over(&mut over, &[flat, flat], 0);
        assert_eq!(over, [0xff60_80a0, 0xffcf_dfef]);
    }

    /// The lift itself: the room at one end, the control screen at the
    /// other, and neither of them anywhere in between.
    #[test]
    fn a_lift_starts_on_the_room_ends_on_the_screen_and_reverses() {
        let room: Vec<u32> = vec![0xff20_2020; W * H];
        let screen: Vec<u32> = vec![0xffc0_c0c0; W * H];
        let row = Window {
            x: 2,
            y: 6,
            w: 12,
            h: 3,
            r: 0,
        };
        let lift = Lift {
            row,
            pitch: 4,
            label: Window {
                x: 4,
                y: 6,
                w: 6,
                h: 3,
                r: 0,
            },
            disc: Window {
                x: 2,
                y: 6,
                w: 2,
                h: 2,
                r: 0,
            },
            title: Window {
                x: 1,
                y: 1,
                w: 8,
                h: 3,
                r: 0,
            },
            screen_disc: Window {
                x: 7,
                y: 5,
                w: 2,
                h: 2,
                r: 0,
            },
            header_h: 5,
            cards: [
                Window {
                    x: 1,
                    y: 8,
                    w: 6,
                    h: 6,
                    r: 0,
                },
                Window::default(),
            ],
            footer_y: 14,
            track: [
                Window {
                    x: 2,
                    y: 9,
                    w: 4,
                    h: 4,
                    r: 0,
                },
                Window::default(),
            ],
            fill_h: 2,
            marker_y: -1,
            marker_h: 1,
        };
        let at = |shown, t| {
            let mut pixels = vec![0u32; W * H];
            // The page being arrived at is the screen on the way in and the
            // room on the way out; the panel hands them over the same way.
            let pages = match shown {
                Shown::Arriving => (&screen[..], &room[..]),
                Shown::Leaving => (&room[..], &screen[..]),
            };
            // The name and the icon are always cut from the room, whichever
            // way the transition is going, which is what `lift` does.
            let label = Sprite::cut(&room, W, H, lift.label);
            let disc = Sprite::cut(&room, W, H, lift.disc);
            lift_frame(
                Surface {
                    pixels: &mut pixels,
                    stride: W,
                    width: W,
                    height: H,
                },
                pages,
                lift,
                (&label, &disc),
                shown,
                t,
            );
            pixels
        };
        // Opening: the room, then the screen.
        assert_eq!(at(Shown::Arriving, 0.0), room);
        assert_eq!(at(Shown::Arriving, 1.0), screen);
        for step in 1..4 {
            let frame = at(Shown::Arriving, step as f32 / 4.0);
            assert_ne!(frame, room, "frame {step} never left the room");
            assert_ne!(frame, screen, "frame {step} is already the screen");
        }
        // Closing is the same run backwards: it starts on the screen it is
        // leaving and ends on the room, so the last frame is a whole page
        // either way round.
        assert_eq!(at(Shown::Leaving, 0.0), screen);
        assert_eq!(at(Shown::Leaving, 1.0), room);
        for step in 1..4 {
            let t = step as f32 / 4.0;
            assert_eq!(
                at(Shown::Leaving, t),
                at(Shown::Arriving, 1.0 - t),
                "the close is not the open backwards at {t}"
            );
        }
    }

    /// A sprite is a rectangle of a page put down somewhere else, clipped at
    /// every edge, and with the card it was cut from left behind.
    #[test]
    fn a_sprite_lands_where_it_is_put_and_leaves_its_backing_behind() {
        let page: Vec<u32> = (0..W * H).map(|i| 100 + i as u32).collect();
        let box_ = Window {
            x: 4,
            y: 4,
            w: 3,
            h: 2,
            r: 0,
        };
        let sprite = Sprite::cut(&page, W, H, box_);
        fn surface(pixels: &mut [u32]) -> Surface<'_> {
            Surface {
                pixels,
                stride: W,
                width: W,
                height: H,
            }
        }
        let mut pixels = vec![0u32; W * H];
        sprite.put(&mut surface(&mut pixels), (1, 1), None);
        assert_eq!(&pixels[W + 1..W + 4], &page[4 * W + 4..4 * W + 7]);
        assert_eq!(&pixels[2 * W + 1..2 * W + 4], &page[5 * W + 4..5 * W + 7]);
        assert_eq!(pixels[W], 0);
        assert_eq!(pixels[W + 4], 0);
        assert!(pixels[3 * W..].iter().all(|p| *p == 0));

        // Off every edge in turn: what fits arrives, the rest is dropped and
        // nothing wraps round.
        let mut pixels = vec![0u32; W * H];
        sprite.put(&mut surface(&mut pixels), (-1, -1), None);
        assert_eq!(&pixels[0..2], &page[5 * W + 5..5 * W + 7]);
        assert!(pixels[2..W].iter().all(|p| *p == 0));
        let mut pixels = vec![0u32; W * H];
        sprite.put(
            &mut surface(&mut pixels),
            (W as i32 - 1, H as i32 - 1),
            None,
        );
        assert_eq!(pixels[H * W - 1], page[4 * W + 4]);
        assert!(pixels[..H * W - 1].iter().all(|p| *p == 0));

        // Keyed: the colour the sprite was cut against is not put down, so
        // only the glyphs travel and the card they sat on stays behind.
        let flat = page[4 * W + 5];
        let mut pixels = vec![7u32; W * H];
        sprite.put(&mut surface(&mut pixels), (1, 1), Some(flat));
        assert_eq!(pixels[W + 1], page[4 * W + 4]);
        assert_eq!(pixels[W + 2], 7, "the keyed pixel was put down");
        assert_eq!(pixels[W + 3], page[4 * W + 6]);
    }

    /// A filled rounded rectangle: the card, rising. The same row runs the
    /// ring's outline uses, filled, with a pixel of edge all round.
    #[test]
    fn a_filled_window_has_an_edge_and_rounded_corners() {
        let mut pixels = vec![0u32; W * H];
        let mut dst = Surface {
            pixels: &mut pixels,
            stride: W,
            width: W,
            height: H,
        };
        fill_window(
            &mut dst,
            Window {
                x: 4,
                y: 5,
                w: 8,
                h: 6,
                r: 2,
            },
            1,
            2,
        );
        let picture: Vec<String> = pixels
            .chunks(W)
            .map(|row| {
                row.iter()
                    .map(|p| match *p {
                        0 => '.',
                        1 => '#',
                        _ => 'o',
                    })
                    .collect()
            })
            .collect();
        assert_eq!(
            picture,
            [
                "................",
                "................",
                "................",
                "................",
                "................",
                ".....oooooo.....",
                "....o######o....",
                "....o######o....",
                "....o######o....",
                "....o######o....",
                ".....oooooo.....",
                "................",
                "................",
                "................",
                "................",
                "................",
            ]
        );
    }

    #[test]
    fn a_curtain_is_the_rows_band_across_the_whole_panel() {
        let row = Window {
            x: 20,
            y: 199,
            w: 440,
            h: 103,
            r: 14,
        };
        assert_eq!(Transition::default().from_row(row, 480), row);
        let curtain = Transition::parse("curtain").from_row(row, 480);
        assert_eq!(
            curtain,
            Window {
                x: 0,
                y: 199,
                w: 480,
                h: 103,
                r: 14
            }
        );
    }

    const W: usize = 16;
    const H: usize = 16;
    const A: u32 = 0xffff_0000;
    const B: u32 = 0xff00_ff00;

    /// One composed frame as a picture: `.` is page A, `#` is page B and `o`
    /// is the ring, one character a pixel.
    fn frame(window: Window, ring: Option<i32>, shown: Shown) -> Vec<String> {
        let (a, b) = (vec![A; W * H], vec![B; W * H]);
        let (outside, inside) = match shown {
            Shown::Arriving => (&a, &b),
            Shown::Leaving => (&b, &a),
        };
        let mut pixels = vec![0u32; W * H];
        let mut dst = Surface {
            pixels: &mut pixels,
            stride: W,
            width: W,
            height: H,
        };
        compose_window(&mut dst, outside, inside, window);
        if let Some(bleed) = ring {
            stroke_window(&mut dst, window.grown(bleed), 1, IRIS_RING);
        }
        pixels
            .chunks(W)
            .map(|row| {
                row.iter()
                    .map(|p| match *p {
                        A => '.',
                        B => '#',
                        IRIS_RING => 'o',
                        _ => '?',
                    })
                    .collect()
            })
            .collect()
    }

    /// The window is exactly the rect it is given, with the corners taken off
    /// by the radius and nothing anywhere else.
    #[test]
    fn the_window_shows_the_arriving_page_through_the_rect_it_is_given() {
        // The first frame of an open: page A everywhere but the row.
        let row = Window {
            x: 4,
            y: 6,
            w: 8,
            h: 4,
            r: 2,
        };
        assert_eq!(
            frame(row, None, Shown::Arriving),
            [
                "................",
                "................",
                "................",
                "................",
                "................",
                "................",
                ".....######.....",
                "....########....",
                "....########....",
                ".....######.....",
                "................",
                "................",
                "................",
                "................",
                "................",
                "................",
            ]
        );
        // The last frame of an open: the arriving page, whole. `iris` reaches
        // it by composing with an empty window, which is what this is.
        assert_eq!(
            frame(Window::default(), None, Shown::Arriving),
            vec![".".repeat(W); H]
        );
        assert_eq!(
            frame(Window::panel(W as u32, H as u32), None, Shown::Arriving),
            vec!["#".repeat(W); H]
        );
    }

    /// A frame from the middle of the travel: the window is bigger, its
    /// corners are still cut, and everything outside it is still page A.
    #[test]
    fn a_frame_in_the_middle_is_the_new_page_inside_the_old_one_outside() {
        let row = Window {
            x: 4,
            y: 6,
            w: 8,
            h: 4,
            r: 2,
        };
        let whole = Window::panel(W as u32, H as u32);
        // Halfway there, every number is halfway there, radius included.
        assert_eq!(
            row.lerp(whole, 0.5),
            Window {
                x: 2,
                y: 3,
                w: 12,
                h: 10,
                r: 1
            }
        );
        // Drawn with a radius worth seeing: page B inside, page A outside,
        // and the corners cut into it.
        let half = Window {
            x: 2,
            y: 3,
            w: 12,
            h: 10,
            r: 4,
        };
        assert_eq!(
            frame(half, None, Shown::Arriving),
            [
                "................",
                "................",
                "................",
                "....########....",
                "...##########...",
                "...##########...",
                "..############..",
                "..############..",
                "..############..",
                "..############..",
                "...##########...",
                "...##########...",
                "....########....",
                "................",
                "................",
                "................",
            ]
        );
        // The reverse is the mirror: the page that is leaving is what shows
        // through the window, and the page behind it is what surrounds it.
        assert_eq!(
            frame(half, None, Shown::Leaving),
            frame(half, None, Shown::Arriving)
                .iter()
                .map(|row| row.replace('#', "@").replace('.', "#").replace('@', "."))
                .collect::<Vec<_>>()
        );
    }

    /// The ring is an outline on the window's edge, one pixel outside it
    /// here, and it never blends: every pixel it touches is the ring colour.
    #[test]
    fn the_ring_is_an_outline_around_the_window_and_nothing_else() {
        let row = Window {
            x: 5,
            y: 6,
            w: 6,
            h: 4,
            r: 1,
        };
        assert_eq!(
            frame(row, Some(1), Shown::Arriving),
            [
                "................",
                "................",
                "................",
                "................",
                "................",
                ".....oooooo.....",
                "....o######o....",
                "....o######o....",
                "....o######o....",
                "....o######o....",
                ".....oooooo.....",
                "................",
                "................",
                "................",
                "................",
                "................",
            ]
        );
    }

    /// The row loop's arithmetic, on its own: no run above or below the
    /// window, the full width in the middle, an inset at the corners, and
    /// everything clamped to the panel once the window grows past it.
    #[test]
    fn a_rows_run_is_inset_at_the_corners_and_clamped_to_the_panel() {
        let window = Window {
            x: 4,
            y: 4,
            w: 8,
            h: 8,
            r: 3,
        };
        assert_eq!(window_run(window, 3, W), None);
        assert_eq!(window_run(window, 12, W), None);
        assert_eq!(window_run(window, 7, W), Some((4, 12)));
        // The first row is the deepest into the corner, and the inset shrinks
        // to nothing by the time the rows clear it.
        let insets: Vec<usize> = (4..8)
            .map(|y| window_run(window, y, W).unwrap().0 - 4)
            .collect();
        assert_eq!(insets, [2, 1, 0, 0]);
        // Symmetric top to bottom.
        for y in 0..4 {
            assert_eq!(window_run(window, 4 + y, W), window_run(window, 11 - y, W));
        }
        // Past the edges of the panel the run is the panel, not more.
        let big = Window {
            x: -20,
            y: -20,
            w: 60,
            h: 60,
            r: 6,
        };
        assert_eq!(window_run(big, 0, W), Some((0, W)));
        // An empty window has no run at all, which is how the last frame of a
        // close puts the whole of the page behind it up.
        assert_eq!(window_run(Window::default(), 0, W), None);
    }
}
