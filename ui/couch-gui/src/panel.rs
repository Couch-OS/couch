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
    /// How many scanlines reached the glass, in total and in the worst single
    /// frame. A lift sends only the rows it wrote, so this is the number that
    /// says on the device whether that is working.
    pub rows: u64,
    pub max_rows: u64,
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

/// How long a screen takes to open, and the one number to change if it reads
/// hurried or slow on the device. The lift has more to say than a window
/// opening - rows falling away, a card travelling, a screen arriving piece by
/// piece - and 400 ms is what the owner settled on after watching it on the
/// HA100, where 320 read rushed. Twenty-four frames at 60 Hz, and every phase
/// in the transition is a fraction of it, so this moves them all together.
pub const LIFT: Duration = Duration::from_millis(400);

/// `Theme.bg`, #15130F, packed the way the panel takes it: what a room row
/// falls away to, and what the control screen's pieces arrive over.
const LIFT_BG: u32 = 0xff0f_1315;
/// What a room band falls away to, for the tests that check the panel is
/// never left bare.
#[cfg(test)]
pub(crate) fn lift_background() -> u32 {
    LIFT_BG
}

/// `Theme.surface`, #1F1C17: a card's fill, which is both what the rising
/// card is drawn in and the colour keyed out of the sprites cut from a row.
pub(crate) const LIFT_SURFACE: u32 = 0xff17_1c1f;
/// `Theme.border`, #2C271F, for that card's edge.
const LIFT_BORDER: u32 = 0xff1f_272c;
/// How far below their places a screen's cards start, and how much later the
/// second of a pair arrives than the first.
pub(crate) const LIFT_BARS_DROP: i32 = 14;
pub(crate) const LIFT_BARS_LAG: f32 = 0.08;
/// How long one band of the room takes to fall away, as a fraction of the
/// whole. Short on purpose: it is the only blending in the transition, and
/// only the bands inside their own window are blended, so keeping it near
/// twice the stagger holds the blended part of the panel to about a quarter
/// of it however many rows there are.
/// How long one scanline of the room takes to fade away, and how much later
/// the furthest one starts than the focused row's own does.
///
/// The fade is long on purpose - nothing on this screen may appear or go in
/// one frame, and five frames at the default time is the floor - and the wave
/// is small, because once the fade is much longer than the wave every
/// scanline is fading at once anyway and a bigger wave only makes the room
/// take longer to leave without costing less.
pub(crate) const LIFT_ROWS_FALL: (f32, f32) = (0.02, 0.26);
pub(crate) const LIFT_ROW_WAVE: f32 = 0.02;
/// The focused row's own contents - its second line, its chevron, its ring -
/// go sooner than the rest: the name and the icon are already on their way
/// out of it, and what is left should not still be there underneath them.
pub(crate) const LIFT_FOCUSED_OUT: (f32, f32) = (0.0, 0.34);
/// The phases, as fractions of the transition, named the way the preview this
/// follows names them (scratchpad/transitions, concept "lift").
/// The card rises from the moment the press lands and is gone by the time it
/// gets there. It fades *in* over the same window the row underneath it fades
/// out, so the two cross and the row's second line and chevron are never
/// hidden in one frame; and it fades out across the rise, so page B does not
/// have to lose a plate it never had.
pub(crate) const LIFT_CARD_RISE: (f32, f32) = (0.0, 0.44);
/// The name and the icon leave with it. They start where they already are,
/// so the first frame is the room untouched however they are drawn.
pub(crate) const LIFT_LABEL_FLY: (f32, f32) = (0.0, 0.44);
/// The name and the icon giving way to the screen's own: they fade out as the
/// header fades in, over the same window, so one becomes the other.
pub(crate) const LIFT_HAND_OVER: (f32, f32) = (0.44, 0.70);
/// The cards arrive while the last of the room is still going. The left one
/// overlaps that fade and the right one does not, on purpose: a whole-panel
/// fade is about twenty-four milliseconds on this device and a card another
/// half, so one may sit on top of it and two may not.
pub(crate) const LIFT_CARDS_IN: (f32, f32) = (0.20, 0.48);
/// How much of a card's arrival is spent fading in rather than settling.
/// Every fade here is at least a fifth of the transition, which is five
/// frames at the default time: the floor below which a fade reads as a step.
pub(crate) const LIFT_CARDS_FADE: f32 = 0.9;
pub(crate) const LIFT_STATE_IN: (f32, f32) = (0.30, 0.56);
pub(crate) const LIFT_GROW: (f32, f32) = (0.52, 1.0);
pub(crate) const LIFT_FOOTER_IN: (f32, f32) = (0.66, 0.94);
/// How much earlier everything the screen does happens when the page it is
/// arriving at has very little on it.
///
/// A control screen full of cards can afford to arrive after the room has
/// gone, because what arrives is most of the panel. A page that says only
/// "Connecting to Sonos…" cannot: the room leaves on the same schedule, and
/// between the two there is nothing but the name and the icon in flight -
/// which is the panel looking broken for a third of a second. A sparse page
/// crosses with the room rather than after it.
pub(crate) const LIFT_HASTE: f32 = 0.22;

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
/// remote takes effect on the next press; no file is the lift at [`LIFT`].
/// The iris and the curtain are the same compositor with a different first
/// window and cost the same; the lift blends, and says what it cost in the
/// line it prints when it is over.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Transition {
    pub opening: Opening,
    pub time: Duration,
}

impl Default for Transition {
    /// The lift, at [`LIFT`]: the shape the owner chose after seeing all
    /// three on the device. A screen with no plan of its own has no lift to
    /// run and keeps the slide it always had, so this is the default for the
    /// screens that can honour it and nothing changes for the rest.
    fn default() -> Self {
        Transition {
            opening: Opening::Lift,
            time: LIFT,
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
    /// Where a lift composes, before any of it reaches the panel. The window
    /// shapes write every scanline once and can go straight to the map; a
    /// lift paints a band flat and puts its pieces back in later passes, and
    /// the panel scans out continuously, so painting that in place shows the
    /// half-painted state as a dark bar walking up the screen. Allocated the
    /// first time a lift runs and kept, never per frame.
    back: Vec<Abgr>,
    /// Where each scanline of the room has anything but background on it,
    /// worked out once at the start of a lift and used by all of its frames.
    content: Vec<Line>,
    /// The same for the page that is arriving.
    screen_content: Vec<Line>,
    /// What a lift cut out of the two pages, and the scratch its cards are
    /// built in. Kept between transitions so nothing is allocated per frame.
    art: LiftArt,
    /// Which scanlines the frame just composed actually wrote, and how far
    /// through the frame before it was: between them, the rows that need to
    /// reach the glass at all.
    dirty: Vec<bool>,
    last_t: Option<f32>,
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
            back: Vec::new(),
            content: Vec::new(),
            screen_content: Vec::new(),
            art: LiftArt::default(),
            dirty: Vec::new(),
            last_t: None,
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
        self.last_t = None;
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
            cost.rows += self.height as u64;
            cost.max_rows = self.height as u64;
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
    pub fn lift(&mut self, plan: LiftPlan, shown: Shown, duration: Duration) -> SlideCost {
        let (w, h) = (self.width as usize, self.height as usize);
        // The room is where the name and the icon are cut from, whichever way
        // the transition is going; the screen is where the marker is. Cut
        // once, here, before a frame is composed.
        {
            let (room, screen) = match shown {
                Shown::Arriving => (pixels(&self.spare), pixels(&self.ram)),
                Shown::Leaving => (pixels(&self.ram), pixels(&self.spare)),
            };
            self.art = crate::lift_art(room, screen, w, h, plan);
        }
        // Where the room has anything on it, row by row. One pass over the
        // page here saves a blend over the parts of every row that are the
        // background already, which on a list is most of the panel.
        {
            let (room, screen) = match shown {
                Shown::Arriving => (pixels(&self.spare), pixels(&self.ram)),
                Shown::Leaving => (pixels(&self.ram), pixels(&self.spare)),
            };
            self.content = lift_content(room, w, h);
            self.screen_content = lift_content(screen, w, h);
        }
        // Everything a frame needs, allocated before the clock starts: the
        // buffer a frame is composed in and the scratch each card is built
        // in. A megabyte and a half taken inside the first timed frame is
        // what made an open cost more than a close, and once put a whole
        // frame over thirty milliseconds.
        if self.back.len() != w * h {
            self.back = vec![Abgr::default(); w * h];
        }
        self.transition(Compose::Lift { plan }, shown, duration)
    }

    /// The frame loop every opening shares: eased time, one composed frame,
    /// the same pacing a drawn frame gets, and the same per-frame report, so
    /// two shapes can be compared on one set of numbers.
    fn transition(&mut self, what: Compose, shown: Shown, duration: Duration) -> SlideCost {
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
            let rows = match what {
                Compose::Iris { from, to } => {
                    self.compose_iris(from, to, shown, t);
                    self.height as usize
                }
                Compose::Lift { plan } => self.compose_lift(plan, shown, t),
            } as u64;
            let work = started.elapsed();
            self.pace(started);
            let wait = started.elapsed().saturating_sub(work);
            let (work_us, wait_us) = (work.as_micros() as u64, wait.as_micros() as u64);
            cost.frames += 1;
            cost.work_us += work_us;
            cost.wait_us += wait_us;
            cost.max_us = cost.max_us.max(work_us);
            cost.rows += rows;
            cost.max_rows = cost.max_rows.max(rows);
            if report {
                match what {
                    Compose::Iris { from, to } => {
                        let window = from.lerp(to, ease_out(t));
                        println!(
                            "couch-gui: iris frame {}: {}x{} at {},{} r{}, {work_us} us, {wait_us} us paced",
                            cost.frames, window.w, window.h, window.x, window.y, window.r
                        );
                    }
                    Compose::Lift { plan } => println!(
                        "couch-gui: {} lift frame {}: t {:.2}, {work_us} us, {wait_us} us paced",
                        plan.name, cost.frames, t
                    ),
                }
            }
            if done {
                return cost;
            }
        }
    }

    /// One lift frame into the framebuffer. Returns the rows it sent.
    fn compose_lift(&mut self, plan: LiftPlan, shown: Shown, t: f32) -> usize {
        let (width, height, stride) = (
            self.width as usize,
            self.height as usize,
            self.stride_px as usize,
        );
        if self.dirty.len() != height {
            self.dirty = vec![true; height];
        }
        // At the point the frame is really drawn at, not at the clock: a
        // close is the same plan run backwards.
        let p = through(shown, t);
        changed_rows(&plan, height, p, self.last_t, &mut self.dirty);
        {
            let (arriving, leaving) = (pixels(&self.ram), pixels(&self.spare));
            lift_frame(
                Surface {
                    pixels: pixels_mut(&mut self.back),
                    stride: width,
                    width,
                    height,
                },
                (arriving, leaving),
                plan,
                &mut self.art,
                (&self.content, &self.screen_content),
                shown,
                Frame {
                    t,
                    dirty: &mut self.dirty,
                },
            );
        }
        self.last_t = Some(p);
        // Whole frames only: the panel never holds a half-composed one. Only
        // the rows this frame wrote are sent; the rest are on the glass
        // already, and untouched in the buffer they were composed in.
        present(
            &mut self.map[..],
            stride,
            width,
            height,
            pixels(&self.back),
            Some(&self.dirty),
        )
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

/// The same view, to write into.
fn pixels_mut(buf: &mut [Abgr]) -> &mut [u32] {
    unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u32, buf.len()) }
}

/// A composed frame onto the panel: one copy a scanline, top to bottom, and
/// every pixel written at most once.
///
/// What makes a multi-pass transition safe to show. The panel is scanned out
/// continuously and nothing here flips buffers, so whatever is in the map is
/// what is on the glass; a frame that is painted in several passes has to be
/// finished somewhere else first, or its intermediate states are seen.
///
/// `rows` says which scanlines the compose actually wrote. The rest are
/// already on the glass from the frame before - the compositor left them
/// alone in its own buffer too - so copying them again is a whole panel of
/// writes to uncached memory for nothing. It was the single largest term in
/// the cost of a frame: more than the blending the shape is named for.
/// Passing `None` writes every row, which is what an opening that composes
/// the whole panel every frame wants.
///
/// Returns how many rows reached the glass, for the line the loop prints.
fn present(
    map: &mut [u32],
    stride: usize,
    width: usize,
    height: usize,
    from: &[u32],
    rows: Option<&[bool]>,
) -> usize {
    let mut written = 0;
    for y in 0..height {
        if rows.is_some_and(|rows| !rows.get(y).copied().unwrap_or(true)) {
            continue;
        }
        map[y * stride..y * stride + width].copy_from_slice(&from[y * width..(y + 1) * width]);
        written += 1;
    }
    charge(1, written * width);
    written
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
enum Compose {
    Iris { from: Window, to: Window },
    Lift { plan: LiftPlan },
}

/// A rectangle of the room that flies to a rectangle of the screen.
///
/// The device's name and its icon: they exist on both pages, so they are not
/// faded anywhere - they are cut out of the room once and put down at an
/// interpolated place until they land on the screen's own, which every screen
/// draws the same way so that the landing is nothing at all.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Traveller {
    pub from: Window,
    pub to: Window,
    /// A colour in `from` that is not carried: the plate it sat on.
    pub key: u32,
    pub fly: (f32, f32),
}

/// How a piece of the arriving screen turns up.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Arriving {
    /// It fades in where it belongs. The cheapest kind: the fade touches only
    /// the part of each row that has anything on it.
    Fade,
    /// It fades in while rising the last few pixels into place.
    Rise(i32),
    /// A card that rises, and whose level fills from the bottom and whose
    /// marker slides up as it settles. Both are already in the page at their
    /// values, so neither is drawn: they are revealed out of it.
    Card {
        rise: i32,
        track: Window,
        fill_h: i32,
        marker_y: i32,
        marker_h: i32,
        grow: (f32, f32),
    },
}

/// One piece of the arriving screen, and when it arrives.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Piece {
    pub rect: Window,
    /// When it starts and when it has settled, as fractions of the whole.
    pub window: (f32, f32),
    /// How much of that is spent fading in rather than settling into place.
    pub fade: f32,
    pub kind: Arriving,
}

/// What a screen tells the lift about itself.
///
/// The transition knows nothing about lights or televisions: a screen hands
/// over the rectangles its own layout gives it - through `App`, never written
/// down twice - and the times, as fractions of the whole, at which each of its
/// parts should arrive. Everything else is the same for every screen.
#[derive(Copy, Clone, Debug, Default)]
pub struct LiftPlan {
    /// Which screen this is, for the line the frame loop prints when it is
    /// over: one transition serves them all, so the numbers have to say which
    /// one they are for.
    pub name: &'static str,
    /// The row it opens out of.
    pub row: Window,
    /// When the room falls away, and how much later the furthest scanline
    /// starts than the row's own does.
    pub fall: (f32, f32),
    pub wave: f32,
    /// When the focused row's own contents go - sooner than the rest, because
    /// what travels is already leaving it.
    pub focused_out: (f32, f32),
    /// The row's card, rising into the header band and fading as it goes.
    pub plate: Option<(Window, (f32, f32))>,
    pub travellers: [Option<Traveller>; 2],
    /// When the travellers give way to the screen's own.
    pub hand_over: (f32, f32),
    pub pieces: [Option<Piece>; 6],
}

impl LiftPlan {
    /// A plan with the choreography every screen shares: the room falling
    /// away outwards from the row, and the row's card rising into the header
    /// band and fading as it goes. A screen fills in what is its own - what
    /// travels out of the row, and the pieces it arrives in.
    pub fn out_of(name: &'static str, row: Window) -> Self {
        Self {
            name,
            row,
            fall: LIFT_ROWS_FALL,
            wave: LIFT_ROW_WAVE,
            focused_out: LIFT_FOCUSED_OUT,
            plate: None,
            travellers: [None; 2],
            hand_over: LIFT_HAND_OVER,
            pieces: [None; 6],
        }
    }

    /// Where the row's own card comes to rest as it rises out of the list and
    /// fades: the screen's own rectangle, read from its header, so that no
    /// screen's geometry is written down here.
    pub fn rising_to(mut self, plate: Window, radius: i32) -> Self {
        self.plate = Some((Window { r: radius, ..plate }, LIFT_CARD_RISE));
        self
    }

    /// The name and the icon, flying from where the row draws them to where
    /// the screen draws its own. Both ends come from the layouts themselves,
    /// so a traveller lands on itself rather than on another drawing of the
    /// same thing.
    pub fn carrying(mut self, name: (Window, Window), icon: (Window, Window)) -> Self {
        let fly = |(from, to): (Window, Window)| {
            Some(Traveller {
                from,
                to,
                // The card they sat on is not carried with them.
                key: LIFT_SURFACE,
                fly: LIFT_LABEL_FLY,
            })
        };
        self.travellers = [fly(name), fly(icon)];
        self
    }

    /// The header: the state line under the disc, which has nothing to wait
    /// for, and then the band itself, which cannot arrive before the name
    /// flying towards it has given way.
    pub fn header(self, width: i32, header_h: i32, under_disc: i32) -> Self {
        let band = |y: i32, h: i32, window| Piece {
            rect: Window {
                x: 0,
                y,
                w: width,
                h,
                r: 0,
            },
            window,
            fade: 1.0,
            kind: Arriving::Fade,
        };
        self.piece(band(under_disc, header_h - under_disc, LIFT_STATE_IN))
            .piece(band(0, header_h, LIFT_HAND_OVER))
    }

    /// The strip along the bottom, last of all.
    pub fn footer(self, width: i32, height: i32, footer_y: i32) -> Self {
        self.piece(Piece {
            rect: Window {
                x: 0,
                y: footer_y,
                w: width,
                h: height - footer_y,
                r: 0,
            },
            window: LIFT_FOOTER_IN,
            fade: 1.0,
            kind: Arriving::Fade,
        })
    }

    /// The screen arrives earlier, without finishing earlier.
    ///
    /// Every piece opens `by` sooner and settles when it always did, so it
    /// crosses with the room rather than following it; what travels lands
    /// `by` sooner instead, because the hand-over cannot begin until it has.
    /// Shifting the whole schedule earlier would only move the empty part of
    /// the transition to the end.
    ///
    /// The room takes `by` longer to go, too. It is the overlap between the
    /// two that keeps the panel occupied, and on a page this empty the room
    /// is the only thing with anything on it for the first third.
    pub fn sooner(mut self, by: f32) -> Self {
        let opens = |w: (f32, f32)| ((w.0 - by).max(0.0), w.1);
        let lands = |w: (f32, f32)| (w.0, (w.1 - by).max(w.0 + 0.02));
        self.fall = (self.fall.0, self.fall.1 + by);
        self.hand_over = opens(self.hand_over);
        if let Some((to, rise)) = self.plate {
            self.plate = Some((to, lands(rise)));
        }
        for traveller in self.travellers.iter_mut().flatten() {
            traveller.fly = lands(traveller.fly);
        }
        for piece in self.pieces.iter_mut().flatten() {
            piece.window = opens(piece.window);
            if let Arriving::Card { grow, .. } = &mut piece.kind {
                *grow = opens(*grow);
            }
        }
        self
    }

    /// One more piece, drawn after the ones already given.
    pub fn piece(mut self, piece: Piece) -> Self {
        if let Some(slot) = self.pieces.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(piece);
        }
        self
    }

    /// The same, for a piece a screen may not have at all.
    pub fn maybe(self, piece: Option<Piece>) -> Self {
        match piece {
            Some(piece) => self.piece(piece),
            None => self,
        }
    }
}

/// A rectangle cut out of a page, kept for the length of one transition.
///
/// Small and owned: a row's name and its icon, a few tens of kilobytes
/// between them, taken once before the first frame and put down at an
/// interpolated place on each. Nothing here allocates once the transition has
/// started.
#[derive(Default)]
pub(crate) struct Sprite {
    pixels: Vec<u32>,
    box_: Window,
}

/// Everything a lift cuts out of the two pages, kept for its length.
///
/// What travels, the band they were handed over from, and one scratch buffer
/// per piece that moves: a card is built up opaque in its own buffer - the
/// page's own card with the fill un-revealed and the marker moved - and only
/// then blended over the frame, once, at the alpha it has reached. Anything
/// painted straight into the frame at its own strength would show through a
/// card that has barely arrived, which is what used to erase the room's rows
/// down the width of the level bar.
#[derive(Default)]
pub(crate) struct LiftArt {
    pub travellers: [Sprite; 2],
    pub handed: Sprite,
    /// One per entry in the plan's `pieces`, and empty for the ones that
    /// fade in place: only a piece that moves is cut.
    pub pieces: [Sprite; 6],
    /// The colour marker at its place in the page, and the gradient just
    /// above it, so it can be moved within a card's own buffer.
    pub marker: Sprite,
    pub under_marker: Sprite,
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
    /// Cut a rectangle out of a page into a sprite that already exists,
    /// keeping whatever it had allocated. Nothing in a transition allocates
    /// after its first frame.
    pub(crate) fn recut(&mut self, page: &[u32], width: usize, height: usize, box_: Window) {
        let (x, y) = (box_.x.max(0) as usize, box_.y.max(0) as usize);
        let w = (box_.w.max(0) as usize).min(width.saturating_sub(x));
        let h = (box_.h.max(0) as usize).min(height.saturating_sub(y));
        self.pixels.clear();
        charge(1, w * h);
        for row in 0..h {
            self.pixels
                .extend_from_slice(&page[(y + row) * width + x..(y + row) * width + x + w]);
        }
        self.box_ = Window {
            x: x as i32,
            y: y as i32,
            w: w as i32,
            h: h as i32,
            r: 0,
        };
    }
    /// Put one rectangle of the page back into the sprite, leaving the rest
    /// of it as it is: what a card needs between frames, because only its
    /// track and its marker change and re-cutting the whole card was half a
    /// panel of copying a frame for nothing.
    pub(crate) fn refresh(&mut self, page: &[u32], width: usize, rect: Window) {
        let left = (rect.x - self.box_.x).clamp(0, self.box_.w) as usize;
        let right = (rect.x + rect.w - self.box_.x).clamp(left as i32, self.box_.w) as usize;
        if left >= right {
            return;
        }
        let sprite_w = self.box_.w as usize;
        for n in 0..self.box_.h {
            let y = self.box_.y + n;
            if y < rect.y || y >= rect.y + rect.h || y < 0 {
                continue;
            }
            let (from, to) = (
                y as usize * width + self.box_.x as usize + left,
                y as usize * width + self.box_.x as usize + right,
            );
            let row = n as usize * sprite_w;
            charge(1, right - left);
            self.pixels[row + left..row + right].copy_from_slice(&page[from..to]);
        }
    }
    /// The sprite as somewhere to compose into, so a card can be built up in
    /// its own buffer before any of it reaches the frame.
    pub(crate) fn surface(&mut self) -> Surface<'_> {
        let (width, height) = (self.box_.w.max(0) as usize, self.box_.h.max(0) as usize);
        Surface {
            pixels: &mut self.pixels,
            stride: width,
            width,
            height,
        }
    }
    /// One row of it, by its offset from the top of the rectangle that was
    /// cut, or nothing because that row is not part of it.
    pub(crate) fn row(&self, n: i32) -> Option<&[u32]> {
        (n >= 0 && n < self.box_.h).then(|| {
            let (n, w) = (n as usize, self.box_.w as usize);
            &self.pixels[n * w..(n + 1) * w]
        })
    }
    /// Paint a rectangle of it out, in page coordinates.
    ///
    /// What makes the row let go of the name and the icon that are flying out
    /// of it: the band it fades away as is a copy of itself with those two
    /// painted in the colour of the card they sat on, so the only ones on the
    /// panel from the first frame are the ones in flight. The colour is the
    /// one the sprites are keyed against, so the glyph edges that were cut
    /// against it land back on exactly it and leave no halo.
    pub(crate) fn paint(&mut self, rect: Window, colour: u32) {
        charge(2, (rect.w.max(0) * rect.h.max(0)) as usize);
        let w = self.box_.w;
        let left = (rect.x - self.box_.x).clamp(0, w) as usize;
        let right = (rect.x + rect.w - self.box_.x).clamp(left as i32, w) as usize;
        if left >= right {
            return;
        }
        for n in 0..self.box_.h {
            let y = self.box_.y + n;
            if y < rect.y || y >= rect.y + rect.h {
                continue;
            }
            let row = n as usize * w as usize;
            self.pixels[row + left..row + right].fill(colour);
        }
    }

    /// Put it down with its top-left corner at `at`, clipped at all four
    /// edges of the surface. `key` is a colour that is not drawn: the card a
    /// row's name sits on, so that only the name travels and not the plate
    /// under it. Written, never read back - a framebuffer is slow to read.
    pub(crate) fn put(&self, dst: &mut Surface<'_>, at: (i32, i32), key: Option<u32>, k: u32) {
        if k == 0 {
            return;
        }
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
            match (key, k >= 256) {
                (None, true) => {
                    charge(1, put.len());
                    put.copy_from_slice(taken)
                }
                (None, false) => blend_over(put, taken, k),
                (Some(key), whole) => {
                    let (kd, ks) = (256 - k, k);
                    for (d, &s) in put.iter_mut().zip(taken) {
                        if s == key {
                            continue;
                        }
                        if whole {
                            *d = s;
                        } else {
                            const LO: u32 = 0x00ff_00ff;
                            const HI: u32 = 0xff00_ff00;
                            let p = *d;
                            let lo = ((p & LO) * kd + (s & LO) * ks) >> 8;
                            let hi = (((p >> 8) & LO) * kd + ((s >> 8) & LO) * ks) & HI;
                            *d = (lo & LO) | hi;
                        }
                    }
                }
            }
        }
    }
}

// What a frame of a transition cost, in pixels touched, so that the tests can
// say where the time goes rather than guess. Blending is several times a copy
// and a copy several times a fill, so the three are counted apart.
#[cfg(test)]
thread_local! {
    pub(crate) static WORK: std::cell::Cell<[usize; 3]> = const {
        std::cell::Cell::new([0; 3])
    };
}
/// 0 blended, 1 copied, 2 filled.
#[cfg(test)]
fn charge(what: usize, pixels: usize) {
    WORK.with(|work| {
        let mut totals = work.get();
        totals[what] += pixels;
        work.set(totals);
    });
}
#[cfg(not(test))]
#[inline(always)]
fn charge(_what: usize, _pixels: usize) {}

/// What one scanline of a page is made of, as far as fading it matters.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct Line {
    /// The half-open span that is not the background. Empty where the row has
    /// nothing on it at all.
    pub from: u32,
    pub to: u32,
    /// The colour of that span where the whole of it is one colour, which on
    /// a list is most of a card's height: fading a flat run to a flat colour
    /// gives another flat colour, so it is a fill rather than a blend, and a
    /// fill is a fraction of the price.
    pub flat: Option<u32>,
}

/// What each scanline of a page is made of, worked out once per transition.
pub(crate) fn lift_content(page: &[u32], width: usize, height: usize) -> Vec<Line> {
    (0..height)
        .map(|y| {
            let line = &page[y * width..(y + 1) * width];
            let Some(from) = line.iter().position(|p| *p != LIFT_BG) else {
                return Line::default();
            };
            let to = line.iter().rposition(|p| *p != LIFT_BG).unwrap_or(from) + 1;
            let first = line[from];
            Line {
                from: from as u32,
                to: to as u32,
                flat: line[from..to].iter().all(|p| *p == first).then_some(first),
            }
        })
        .collect()
}

/// One colour `k` of 256 of the way to another.
fn mix(from: u32, to: u32, k: u32) -> u32 {
    debug_assert!(k <= 256);
    const LO: u32 = 0x00ff_00ff;
    const HI: u32 = 0xff00_ff00;
    let (kt, kf) = (k, 256 - k);
    let lo = ((from & LO) * kf + (to & LO) * kt) >> 8;
    let hi = (((from >> 8) & LO) * kf + ((to >> 8) & LO) * kt) & HI;
    (lo & LO) | hi
}

/// Blend a flat colour into what is already there: `k` of 256 of the colour.
///
/// The one place a frame is read back as well as written - which is safe and
/// cheap because a lift composes in RAM and only the finished frame reaches
/// the panel.
fn tint(dst: &mut [u32], colour: u32, k: u32) {
    debug_assert!(k <= 256);
    charge(0, dst.len());
    const LO: u32 = 0x00ff_00ff;
    const HI: u32 = 0xff00_ff00;
    let (kc, kd) = (k, 256 - k);
    let (c_lo, c_hi) = ((colour & LO) * kc, ((colour >> 8) & LO) * kc);
    for d in dst.iter_mut() {
        let p = *d;
        let lo = ((p & LO) * kd + c_lo) >> 8;
        let hi = (((p >> 8) & LO) * kd + c_hi) & HI;
        *d = (lo & LO) | hi;
    }
}

/// A filled rounded rectangle with a one-pixel edge, in the colours a card is
/// drawn in: the row's card, rising.
///
/// The same row-run arithmetic the focus ring's outline uses, filled instead
/// of stroked, so it costs a run a scanline over a rectangle that is never
/// more than a tenth of the panel.
fn fill_window(dst: &mut Surface<'_>, box_: Window, fill: u32, edge: u32, k: u32) {
    let (w, h, stride) = (dst.width, dst.height, dst.stride);
    if k == 0 {
        return;
    }
    for y in 0..h {
        let Some((left, right)) = window_run(box_, y as i32, w) else {
            continue;
        };
        let row = &mut dst.pixels[y * stride + left..y * stride + right];
        // The edge: the first and last row of the box, and a pixel each side
        // of every row, which follows the corner inset by construction.
        let ends = y as i32 == box_.y || y as i32 == box_.y + box_.h - 1;
        if k >= 256 {
            row.fill(if ends { edge } else { fill });
            if !ends {
                if let Some(first) = row.first_mut() {
                    *first = edge;
                }
                if let Some(last) = row.last_mut() {
                    *last = edge;
                }
            }
            continue;
        }
        if ends {
            tint(row, edge, k);
            continue;
        }
        let width = row.len();
        let last = width.saturating_sub(1);
        tint(&mut row[..1.min(width)], edge, k);
        if last > 0 {
            tint(&mut row[1..last], fill, k);
            tint(&mut row[last..], edge, k);
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
/// Which frame of a transition this is, and which of its scanlines are not
/// the ones the panel is already showing.
///
/// `previous` is how far through the frame before it was, and nothing at all
/// on the first. Between the two, everything the plan does is known at both
/// moments, so whether a row can differ is arithmetic rather than a guess:
/// the room's fade there, the plate, what is in flight, and every piece over
/// it. A row that cannot differ is not composed and not sent - and for most
/// of a transition that is most of the panel, which was the largest single
/// cost in a frame, larger than the blending the shape is named for.
///
/// An empty `dirty` means "assume everything differs", which is what a test
/// composing one frame into its own buffer wants.
pub(crate) struct Frame<'a> {
    pub t: f32,
    pub dirty: &'a mut [bool],
}

impl Frame<'_> {
    /// One frame on its own, with nothing before it and nothing to report.
    #[cfg(test)]
    pub(crate) fn at(t: f32) -> Frame<'static> {
        Frame { t, dirty: &mut [] }
    }
}

/// Which rows of the panel a frame can differ from the one before it in.
///
/// Everything the lift draws is a function of `p`, so this asks the same
/// question of both moments and marks a row where any answer differs. It is
/// deliberately generous: a piece that is mid-arrival marks its rows whether
/// or not the pixels really moved, and a moving thing marks where it was as
/// well as where it is. Being wrong the other way would leave a stale band on
/// the glass, so the test that composes a whole run and compares every frame
/// against one drawn from nothing is the one that holds this honest.
/// How far through the plan a frame is, which is not the clock: a close runs
/// the same plan backwards, so `t` of 0.2 into a close is the plan at 0.8.
/// Everything that asks the plan a question has to ask it here.
pub(crate) fn through(shown: Shown, t: f32) -> f32 {
    match shown {
        Shown::Arriving => t,
        Shown::Leaving => 1.0 - t,
    }
}

fn changed_rows(plan: &LiftPlan, h: usize, p: f32, previous: Option<f32>, into: &mut [bool]) {
    let Some(was) = previous else {
        into.fill(true);
        return;
    };
    // The end of the run is the page itself, copied whole; a frame either
    // side of that is not a step in the plan and has nothing in common with
    // it. A close starts there, so this is its second frame.
    if p >= 1.0 || was >= 1.0 {
        into.fill(true);
        return;
    }
    into.fill(false);
    let mark = |from: i32, rows: i32, into: &mut [bool]| {
        for y in from.max(0)..(from + rows).max(0) {
            if let Some(row) = into.get_mut(y as usize) {
                *row = true;
            }
        }
    };
    // The room falling away, scanline by scanline.
    let centre = plan.row.y + plan.row.h / 2;
    let reach = centre.max(h as i32 - centre).max(1) as f32;
    let band = plan.row.y..plan.row.y + plan.row.h;
    let span = plan.fall.1 - plan.fall.0;
    for y in 0..h {
        let window = if band.contains(&(y as i32)) {
            plan.focused_out
        } else {
            let away = ((y as i32 - centre).abs() as f32 / reach).min(1.0);
            let began = plan.fall.0 + plan.wave * away;
            (began, began + span)
        };
        let settled = |at: f32| fading(at, window).is_some_and(|gone| gone >= 256);
        if !(settled(p) && settled(was)) {
            into[y] = true;
        }
    }
    // The plate, where it is and where it was.
    if let Some((to, rise)) = plan.plate {
        for at in [p, was] {
            if at >= rise.0 && at <= rise.1 {
                let plate = plan.row.lerp(to, smooth(progress(at, rise)));
                mark(plate.y, plate.h, into);
            }
        }
    }
    // What is in flight, likewise - a traveller moves, so where it was has to
    // be laid down again.
    for at in [p, was] {
        if fading(at, plan.hand_over).unwrap_or(0) >= 256 {
            continue;
        }
        for traveller in plan.travellers.iter().flatten() {
            if at < traveller.fly.0 {
                continue;
            }
            let e = smooth(progress(at, traveller.fly));
            let y = traveller.from.y
                + (((traveller.to.y - traveller.from.y) as f32) * e).round() as i32;
            mark(y, traveller.from.h, into);
        }
    }
    // And every piece that is not in the same state at both moments: not
    // begun at either, or whole at both, is a piece that draws the same
    // pixels twice.
    for piece in plan.pieces.iter().flatten() {
        let (now, then) = (arrival(*piece, p), arrival(*piece, was));
        let same = matches!((now, then), (None, None) | (Some(256), Some(256)));
        if !same {
            mark(
                piece.rect.y - LIFT_BARS_DROP,
                piece.rect.h + LIFT_BARS_DROP,
                into,
            );
        }
    }
}

pub(crate) fn lift_frame(
    mut dst: Surface<'_>,
    pages: (&[u32], &[u32]),
    // What the screen being opened says about itself: nothing here knows
    // whether it is a lamp or a television.
    plan: LiftPlan,
    // Everything cut out of the two pages for this transition, including the
    // scratch each moving piece is built up in.
    art: &mut LiftArt,
    // What each page is made of, row by row: the room's, and the arriving
    // screen's, so a fade touches only the part of a row that has anything on
    // it. Worked out once before the first frame.
    content: (&[Line], &[Line]),
    shown: Shown,
    frame: Frame<'_>,
) {
    let t = frame.t;
    let (arriving, leaving) = pages;
    let (content, screen_content) = content;
    let p = through(shown, t);
    let (room, screen) = match shown {
        Shown::Arriving => (leaving, arriving),
        Shown::Leaving => (arriving, leaving),
    };
    let (w, h, stride) = (dst.width, dst.height, dst.stride);
    // The end is the page itself, whole: every piece has arrived and nothing
    // is left to work out.
    if p >= 1.0 {
        for y in 0..h {
            dst.pixels[y * stride..y * stride + w].copy_from_slice(&screen[y * w..(y + 1) * w]);
        }
        frame.dirty.fill(true);
        return;
    }
    // --- the room falls away, outwards from the row -----------------------
    // Every scanline gets its own long fade, a little later the further from
    // the row it is; the focused row's own goes sooner, because its name and
    // its icon are already leaving it and what is left should not sit under
    // them. Before its window a scanline is a copy of the room, after it a
    // fill - and the fill is what the screen's background is anyway.
    let centre = plan.row.y + plan.row.h / 2;
    let reach = centre.max(h as i32 - centre).max(1) as f32;
    let band = plan.row.y..plan.row.y + plan.row.h;
    let span = plan.fall.1 - plan.fall.0;
    for y in 0..h {
        let window = if band.contains(&(y as i32)) {
            plan.focused_out
        } else {
            let away = ((y as i32 - centre).abs() as f32 / reach).min(1.0);
            let began = plan.fall.0 + plan.wave * away;
            (began, began + span)
        };
        // A row that cannot differ from the one on the glass is not composed
        // at all: the buffer already holds it, and the panel already shows it.
        if !frame.dirty.get(y).copied().unwrap_or(true) {
            continue;
        }
        let out = &mut dst.pixels[y * stride..y * stride + w];
        let line_of = content.get(y).copied().unwrap_or(Line {
            from: 0,
            to: w as u32,
            flat: None,
        });
        let (from, to) = (line_of.from as usize, line_of.to as usize);
        // Inside the focused row, the room is the band it handed its name and
        // its icon over from, so they are not in two places at once.
        let line = art
            .handed
            .row(y as i32 - plan.row.y)
            .unwrap_or(&room[y * w..(y + 1) * w]);
        match fading(p, window) {
            None => {
                charge(1, out.len());
                out.copy_from_slice(line)
            }
            Some(gone) if gone < 256 && from < to => {
                // Only the part of the row that has something on it crosses;
                // the rest is already the background it is crossing to. A run
                // that is all one colour crosses to one colour, so it is a
                // fill - which is most of a card's height.
                charge(2, w - (to - from));
                out[..from].fill(LIFT_BG);
                match line_of.flat {
                    Some(colour) => {
                        charge(2, to - from);
                        out[from..to].fill(mix(LIFT_BG, colour, 256 - gone));
                    }
                    None => blend_flat(&mut out[from..to], &line[from..to], LIFT_BG, 256 - gone),
                }
                out[to..].fill(LIFT_BG);
            }
            Some(_) => {
                charge(2, out.len());
                out.fill(LIFT_BG)
            }
        }
    }
    // --- the focused card rises into the header band, fading as it goes ---
    // Gone by the time it lands, as the preview has it: page B has no card
    // behind its title, so a plate that was still there at the end would have
    // to vanish in one frame, which is the thing that must not happen.
    if let Some((to, rise)) = plan.plate {
        if p >= rise.0 && p <= rise.1 {
            let e = smooth(progress(p, rise));
            let arriving = fading(p, plan.focused_out).unwrap_or(256);
            let leaving = 256 - (e * 256.0).round() as u32;
            let plate = plan.row.lerp(to, e);
            fill_window(
                &mut dst,
                plate,
                LIFT_SURFACE,
                LIFT_BORDER,
                arriving.min(leaving),
            );
        }
    }
    // --- the screen arrives, piece by piece, in the order it gave them ----
    for (i, piece) in plan.pieces.iter().enumerate() {
        let Some(piece) = *piece else { continue };
        let Some(e) = phase(p, piece.window) else {
            continue;
        };
        // Everything travels and fades at once: a piece that arrived whole
        // would be a fifth of the panel appearing between two frames.
        let Some(k) = arrival(piece, p) else { continue };
        if k == 0 || covered(&plan, i, piece.rect, p) {
            continue;
        }
        // Every row it covers is one the panel is already showing, so it is
        // already in the buffer exactly as it would be drawn again. Asked of
        // the rows rather than of the piece: if anything at all forced one of
        // them to be laid down again - the room's fade, a traveller passing,
        // a neighbour arriving - then this has to go back on top of it.
        let clean = (piece.rect.y - LIFT_BARS_DROP..piece.rect.y + piece.rect.h + LIFT_BARS_DROP)
            .all(|y| y < 0 || !frame.dirty.get(y as usize).copied().unwrap_or(true));
        if clean {
            continue;
        }
        match piece.kind {
            // The cheapest kind, and the one most of a screen is: the band is
            // faded over the frame touching only the part of each row that
            // has anything on it.
            Arriving::Fade => band_over_content(&mut dst, screen, piece.rect, k, screen_content),
            // The piece is built up opaque in its own buffer first and only
            // then blended over the frame, once, at the alpha it has reached.
            // Nothing of it is ever written into the frame at its own
            // strength, or a piece that has barely arrived would still erase
            // what is behind it.
            Arriving::Rise(by) | Arriving::Card { rise: by, .. } => {
                if let Arriving::Card { .. } = piece.kind {
                    build_card(art, screen, w, piece, i, p);
                }
                let dy = (by as f32 * (1.0 - e)).round() as i32;
                art.pieces[i].put(&mut dst, (piece.rect.x, piece.rect.y + dy), None, k);
            }
        }
    }
    // --- its name and its icon fly to the title and the disc --------------
    // Keyed on the card they were cut from, so only the glyphs travel, and
    // they give way to the screen's own over the hand-over rather than
    // stopping: the two are drawn the same way, so it reads as one thing.
    // Last, so nothing that arrives behind them is ever drawn over them.
    let handing = fading(p, plan.hand_over).unwrap_or(0);
    if handing < 256 {
        // Back to front: they cross each other on their way out of the row,
        // and the name is the one that should be whole when they do.
        for (i, traveller) in plan.travellers.iter().enumerate().rev() {
            let Some(fly) = *traveller else { continue };
            if p < fly.fly.0 {
                continue;
            }
            let e = smooth(progress(p, fly.fly));
            let at = (
                fly.from.x + (((fly.to.x - fly.from.x) as f32) * e).round() as i32,
                fly.from.y + (((fly.to.y - fly.from.y) as f32) * e).round() as i32,
            );
            art.travellers[i].put(&mut dst, at, Some(fly.key), 256 - handing);
        }
    }
}

/// How much of a piece is on the panel `p` of the way through, in 0..=256, or
/// nothing because it has not begun. Every piece spends the first part of its
/// window fading in and the rest settling into place.
fn arrival(piece: Piece, p: f32) -> Option<u32> {
    let span = piece.window.1 - piece.window.0;
    phase(p, piece.window)
        .map(|_| fading(p, (piece.window.0, piece.window.0 + span * piece.fade)).unwrap_or(256))
}

/// Whether a later piece has covered this one outright: the state line sits
/// inside the header band, and once the header is whole it is the header that
/// is on the panel. Fading one under the other is work for nothing.
fn covered(plan: &LiftPlan, i: usize, rect: Window, p: f32) -> bool {
    plan.pieces.iter().skip(i + 1).flatten().any(|later| {
        matches!(later.kind, Arriving::Fade)
            && arrival(*later, p) == Some(256)
            && later.rect.x <= rect.x
            && later.rect.y <= rect.y
            && later.rect.x + later.rect.w >= rect.x + rect.w
            && later.rect.y + later.rect.h >= rect.y + rect.h
    })
}

/// How far through a window `p` is, 0 before it and 1 after: the raw fraction,
/// for the ramps that ease themselves.
fn progress(p: f32, window: (f32, f32)) -> f32 {
    ((p - window.0) / (window.1 - window.0).max(f32::EPSILON)).clamp(0.0, 1.0)
}

/// Each card on screen `t` of the way through, with the alpha it has reached.
/// For the test that holds the rule that nothing inside an arriving card is
/// stronger than the card.
#[cfg(test)]
pub(crate) fn lift_cards(plan: LiftPlan, t: f32) -> Vec<(usize, Window, u32)> {
    plan.pieces
        .iter()
        .enumerate()
        .filter_map(|(i, piece)| Some((i, (*piece)?)))
        .filter(|(_, piece)| matches!(piece.kind, Arriving::Card { .. }))
        .filter_map(|(i, piece)| Some((i, piece.rect, arrival(piece, t)?)))
        .collect()
}

/// What is travelling in a lift frame, element by element, and `None` for an
/// element that is not on screen at all at that moment.
///
/// For the test that holds the rule that nothing appears or disappears in one
/// frame: a rectangle that is in both of two consecutive frames has moved and
/// may change as much as it likes, and one that is in only one of them is a
/// thing that popped. A piece that only fades is not travelling and is not
/// excused - it is exactly what that rule is for.
#[cfg(test)]
pub(crate) fn lift_pieces(plan: LiftPlan, t: f32) -> [Option<Window>; 10] {
    let mut pieces = [None; 10];
    if let Some((to, rise)) = plan.plate {
        if t >= rise.0 {
            pieces[0] = Some(plan.row.lerp(to, smooth(progress(t, rise))));
        }
    }
    for (i, traveller) in plan.travellers.iter().enumerate() {
        let Some(fly) = *traveller else { continue };
        if t < fly.fly.0 {
            continue;
        }
        let e = smooth(progress(t, fly.fly));
        pieces[1 + i] = Some(Window {
            x: fly.from.x + (((fly.to.x - fly.from.x) as f32) * e).round() as i32,
            y: fly.from.y + (((fly.to.y - fly.from.y) as f32) * e).round() as i32,
            ..fly.from
        });
    }
    // The level a card shows and the colour it shows grow the whole way, so
    // the tracks they grow in are always allowed to differ from one frame to
    // the next.
    let mut growing: Option<Window> = None;
    for (i, piece) in plan.pieces.iter().enumerate() {
        let Some(piece) = *piece else { continue };
        if let Arriving::Card { track, .. } = piece.kind {
            if track.w > 0 {
                growing = Some(match growing {
                    Some(so_far) => Window {
                        x: so_far.x.min(track.x),
                        y: so_far.y.min(track.y),
                        w: (so_far.x + so_far.w).max(track.x + track.w) - so_far.x.min(track.x),
                        h: (so_far.y + so_far.h).max(track.y + track.h) - so_far.y.min(track.y),
                        r: 0,
                    },
                    None => track,
                });
            }
        }
        if !matches!(piece.kind, Arriving::Fade) && phase(t, piece.window).is_some() {
            pieces[3 + i] = Some(piece.rect);
        }
    }
    pieces[9] = growing;
    pieces
}

/// An alpha ramp with no step at either end: flat where it starts and where
/// it stops, so a fade begins and finishes without a visible first or last
/// jump. `ease_out` is right for something travelling and wrong for something
/// appearing - it opens at its fastest.
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How much of something is on screen `p` of the way through, as an alpha in
/// 0..=256, or nothing at all because its window has not opened.
fn fading(p: f32, window: (f32, f32)) -> Option<u32> {
    (p >= window.0).then(|| {
        let span = (window.1 - window.0).max(f32::EPSILON);
        (smooth((p - window.0) / span) * 256.0).round() as u32
    })
}

/// How far through a phase `p` is, or nothing because it has not begun.
/// Eased the way everything else here is once it has.
fn phase(p: f32, window: (f32, f32)) -> Option<f32> {
    (p >= window.0).then(|| {
        let span = (window.1 - window.0).max(f32::EPSILON);
        ease_out(((p - window.0) / span).clamp(0.0, 1.0))
    })
}

/// The card as it looks this frame, built up opaque in its own buffer: the
/// page's own card, with the level bar filled only as far as it has grown and
/// the colour marker where it has slid to.
///
/// Neither is drawn from nothing - the page already holds both at their
/// values. The fill is un-revealed from the top by painting the empty part of
/// the track in the colour the page gives it, and the marker is moved by
/// putting the gradient that lives above it over its place and putting the
/// marker itself where it has got to. All of it inside the buffer, so the one
/// thing that reaches the frame is the card, at the card's own alpha.
fn build_card(art: &mut LiftArt, screen: &[u32], width: usize, piece: Piece, i: usize, p: f32) {
    let Arriving::Card {
        track,
        fill_h,
        marker_y,
        marker_h,
        grow,
        ..
    } = piece.kind
    else {
        return;
    };
    if track.w <= 0 {
        return;
    }
    let card = piece.rect;
    // The card was cut whole before the first frame; only its track changes,
    // so only its track is put back from the page before it is changed again.
    art.pieces[i].refresh(
        screen,
        width,
        Window {
            x: track.x,
            y: track.y - marker_h,
            w: track.w,
            h: track.h + marker_h,
            r: 0,
        },
    );
    let grown = phase(p, grow).unwrap_or(0.0);
    if fill_h > 0 {
        // The colour of an empty track, from the page's own top of it.
        let sample = screen
            .get(((track.y + 2).max(0) as usize) * width + (track.x + track.w / 2).max(0) as usize)
            .copied()
            .unwrap_or(LIFT_BG);
        let risen = (fill_h as f32 * grown).round() as i32;
        art.pieces[i].paint(
            Window {
                x: track.x,
                y: track.y,
                w: track.w,
                h: track.h - risen,
                r: 0,
            },
            sample,
        );
    } else if marker_y >= 0 {
        let foot = track.y + track.h - marker_h;
        let at = foot + ((marker_y - foot) as f32 * grown).round() as i32;
        if at == marker_y {
            return;
        }
        // Where the page keeps it, put back as the gradient just above it -
        // which is the same gradient, a few mirek along.
        let home = (track.x - card.x, marker_y - card.y);
        let (under, marker) = (&art.under_marker, &art.marker);
        let mut into = art.pieces[i].surface();
        under.put(&mut into, home, None, 256);
        marker.put(&mut into, (home.0, at - card.y), None, 256);
    }
}

/// A band of the arriving page faded over the frame, touching only the part
/// of each row that has anything on it.
///
/// The header and the footer of a screen are mostly background, and fading
/// background over background is work for nothing.
fn band_over_content(dst: &mut Surface<'_>, src: &[u32], band: Window, k: u32, content: &[Line]) {
    let (w, h, stride) = (dst.width, dst.height, dst.stride);
    if k == 0 {
        return;
    }
    for row in 0..band.h.max(0) {
        let y = band.y + row;
        if y < 0 || y >= h as i32 {
            continue;
        }
        let y = y as usize;
        let line = content.get(y).copied().unwrap_or(Line {
            from: 0,
            to: w as u32,
            flat: None,
        });
        let (from, to) = (
            (line.from as usize).max(band.x.max(0) as usize),
            (line.to as usize).min((band.x + band.w).max(0) as usize),
        );
        if from >= to {
            continue;
        }
        let put = &mut dst.pixels[y * stride + from..y * stride + to];
        let taken = &src[y * w + from..y * w + to];
        if k >= 256 {
            charge(1, put.len());
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
/// The u32-at-a-time version of this measured about 20 ms for a whole panel
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

/// The rules a lift has to hold, whatever screen it is opening.
///
/// Here rather than beside one screen's test because every screen that adopts
/// the lift has to pass the same five, and a rule that lives inside one test
/// only ever guards one screen. Each takes the two pages as the panel really
/// draws them and the plan the screen really hands over, so what is checked is
/// the transition the device will run and not a model of it.
#[cfg(test)]
pub(crate) mod checks {
    use super::*;

    /// How many frames a default-length lift actually draws on a 60 Hz panel.
    /// Derived rather than written down, so changing [`LIFT`] moves every
    /// check with it.
    pub(crate) const FRAMES: usize = (LIFT.as_millis() / FRAME.as_millis()) as usize;

    /// Where the time goes, frame by frame, in pixels touched. A blend is
    /// several times a copy and a copy several times a fill, so a budget
    /// in "panels of blending" is the honest unit: the HA100 measured a
    /// whole panel of blending at about twenty-four milliseconds and a whole
    /// panel copied at about three (docs/slint-notes.md).
    pub(crate) fn profile(
        room: &[u32],
        screen: &[u32],
        panel_w: usize,
        panel_h: usize,
        plan: LiftPlan,
        what: &str,
    ) -> (f32, f32) {
        let mut art = crate::lift_art(room, screen, panel_w, panel_h, plan);
        let content = lift_content(room, panel_w, panel_h);
        let screen_content = lift_content(screen, panel_w, panel_h);
        let (mut worst, mut total) = (0.0f32, 0.0f32);
        // As the panel really composes: one buffer across the whole run, each
        // frame knowing how far through the one before it was, and only the
        // rows it wrote reaching the glass.
        let mut pixels = vec![0u32; panel_w * panel_h];
        let mut dirty = vec![false; panel_h];
        let mut previous = None;
        for step in 0..=FRAMES {
            let t = step as f32 / FRAMES as f32;
            changed_rows(&plan, panel_h, t, previous, &mut dirty);
            WORK.with(|w| w.set([0; 3]));
            lift_frame(
                Surface {
                    pixels: &mut pixels,
                    stride: panel_w,
                    width: panel_w,
                    height: panel_h,
                },
                (screen, room),
                plan,
                &mut art,
                (&content, &screen_content),
                Shown::Arriving,
                Frame {
                    t,
                    dirty: &mut dirty,
                },
            );
            previous = Some(t);
            // The present pass the panel does after every composed frame -
            // only the rows this one wrote.
            WORK.with(|w| {
                let mut c = w.get();
                c[1] += dirty.iter().filter(|row| **row).count() * panel_w;
                w.set(c);
            });
            let [blended, copied, filled] = WORK.with(|w| w.get());
            // A copy is about an eighth of a blend and a fill about a
            // sixteenth, on the numbers from the device.
            let cost = (blended as f32 + copied as f32 / 8.0 + filled as f32 / 16.0)
                / (panel_w * panel_h) as f32;
            total += cost;
            worst = worst.max(cost);
            println!(
                "PROFILE {what} t={t:.2} blended={blended} copied={copied} \
                     filled={filled} panels={cost:.2}"
            );
        }
        println!(
            "PROFILE {what} mean={:.2} worst={worst:.2} panels of blending",
            total / (FRAMES + 1) as f32
        );
        (total / (FRAMES + 1) as f32, worst)
    }

    /// The panel is never nearly empty on the way across.
    ///
    /// Measured against the pages themselves rather than against a flat
    /// percentage: a control screen is mostly background and a "Connecting…"
    /// page is almost all background, so "no more than 92% of the panel is
    /// the background colour" says nothing about a page that is 95% background
    /// when it has finished arriving. What matters is how much of what the
    /// screen will hold is already there, so every frame is held against the
    /// emptier of the two pages it is between.
    ///
    /// Returns the worst share any frame reached and when, so a plan can be
    /// measured before a threshold is put on it; `never_bare` puts the
    /// threshold on it.
    pub(crate) fn bareness(
        room: &[u32],
        screen: &[u32],
        panel_w: usize,
        panel_h: usize,
        plan: LiftPlan,
    ) -> (f32, f32) {
        let ink = |page: &[u32]| page.iter().filter(|p| **p != LIFT_BG).count();
        let floor = ink(room).min(ink(screen)).max(1) as f32;
        let mut art = crate::lift_art(room, screen, panel_w, panel_h, plan);
        let content = lift_content(room, panel_w, panel_h);
        let screen_content = lift_content(screen, panel_w, panel_h);
        let (mut worst, mut at) = (f32::MAX, 0.0);
        for step in 0..=FRAMES {
            let t = step as f32 / FRAMES as f32;
            let mut pixels = vec![0u32; panel_w * panel_h];
            lift_frame(
                Surface {
                    pixels: &mut pixels,
                    stride: panel_w,
                    width: panel_w,
                    height: panel_h,
                },
                (screen, room),
                plan,
                &mut art,
                (&content, &screen_content),
                Shown::Arriving,
                Frame::at(t),
            );
            let share = ink(&pixels) as f32 / floor;
            if share < worst {
                worst = share;
                at = t;
            }
        }
        (worst, at)
    }

    /// The panel always holds a quarter of what the emptier of the two pages
    /// holds.
    ///
    /// A quarter rather than a half because the floor case is real: a
    /// "Connecting to Sonos…" page is one line and one button, and while the
    /// room is leaving there is genuinely not much to put on the panel. A
    /// light screen reaches 0.52 and a television 0.67; the player's waiting
    /// page reached 0.17 before it learnt to cross with the room instead of
    /// following it, which is the frame this is here to catch.
    pub(crate) fn never_bare(
        room: &[u32],
        screen: &[u32],
        panel_w: usize,
        panel_h: usize,
        plan: LiftPlan,
        what: &str,
    ) {
        let (share, at) = bareness(room, screen, panel_w, panel_h, plan);
        assert!(
            share >= 0.25,
            "{what}: at {at:.2} the panel held {share:.2} of what the emptier of the two \
             pages holds - the room had gone and the screen had not arrived"
        );
    }

    /// Nothing appears or disappears in one frame. Over the frames a
    /// default-length lift actually draws, no patch of the panel may change
    /// by much from one to the next unless it is a piece that is travelling,
    /// and a piece counts as travelling only if it is on screen in both
    /// frames - so a thing that vanished is not excused by having moved.
    pub(crate) fn pops(
        room: &[u32],
        screen: &[u32],
        panel_w: usize,
        panel_h: usize,
        plan: LiftPlan,
        what: &str,
    ) {
        let mut art = crate::lift_art(room, screen, panel_w, panel_h, plan);
        let content = lift_content(room, panel_w, panel_h);
        let screen_content = lift_content(screen, panel_w, panel_h);
        let mut frame = |t: f32| {
            let mut pixels = vec![0u32; panel_w * panel_h];
            lift_frame(
                Surface {
                    pixels: &mut pixels,
                    stride: panel_w,
                    width: panel_w,
                    height: panel_h,
                },
                (screen, room),
                plan,
                &mut art,
                (&content, &screen_content),
                Shown::Arriving,
                Frame::at(t),
            );
            pixels
        };
        const TILE: (usize, usize) = (32, 16);
        let tiles = (panel_w.div_ceil(TILE.0), panel_h.div_ceil(TILE.1));
        // For every patch of the panel: how much it changed in its worst
        // single frame, and how much it changed over the whole
        // transition. A thing that fades spreads its change over many
        // frames and no one of them is most of it; a thing that is cut
        // puts all of it in one. That ratio is the test, and it does not
        // care whether the thing is a bright glyph or a card a shade
        // lighter than the background.
        let mut worst_step = vec![0u32; tiles.0 * tiles.1];
        let mut total = vec![0u32; tiles.0 * tiles.1];
        let mut before = frame(0.0);
        for step in 1..=FRAMES {
            let t = step as f32 / FRAMES as f32;
            let after = frame(t);
            let travelling: Vec<Window> = lift_pieces(plan, (step - 1) as f32 / FRAMES as f32)
                .iter()
                .zip(lift_pieces(plan, t))
                .filter_map(|(a, b)| match (a, b) {
                    (Some(a), Some(b)) => Some(Window {
                        x: a.x.min(b.x),
                        y: a.y.min(b.y),
                        w: (a.x + a.w).max(b.x + b.w) - a.x.min(b.x),
                        h: (a.y + a.h).max(b.y + b.h) - a.y.min(b.y),
                        r: 0,
                    }),
                    _ => None,
                })
                .collect();
            for ty in 0..tiles.1 {
                for tx in 0..tiles.0 {
                    let (x0, y0) = (tx * TILE.0, ty * TILE.1);
                    // A piece that is on screen in both frames has moved,
                    // and may change as much as it likes; one that is in
                    // only one of them is exactly what this looks for, so
                    // it is not excused.
                    let moving = travelling.iter().any(|piece| {
                        x0 as i32 + TILE.0 as i32 > piece.x
                            && (x0 as i32) < piece.x + piece.w
                            && y0 as i32 + TILE.1 as i32 > piece.y
                            && (y0 as i32) < piece.y + piece.h
                    });
                    let (mut sum, mut n) = (0u32, 0u32);
                    for y in y0..(y0 + TILE.1).min(panel_h) {
                        for x in x0..(x0 + TILE.0).min(panel_w) {
                            let (a, b) = (before[y * panel_w + x], after[y * panel_w + x]);
                            for shift in [0, 8, 16] {
                                sum += ((a >> shift) & 0xff).abs_diff((b >> shift) & 0xff);
                                n += 1;
                            }
                        }
                    }
                    let mean = sum / n.max(1);
                    let tile = ty * tiles.0 + tx;
                    // Everything a patch ever does counts towards its
                    // total, including while a sprite is over it; only
                    // the frames it was left to itself are judged.
                    total[tile] += mean;
                    if !moving {
                        worst_step[tile] = worst_step[tile].max(mean);
                    }
                }
            }
            before = after;
        }
        // A patch that barely moved at all over the whole transition is
        // not worth judging: rounding alone would trip it.
        let popped = (0..worst_step.len())
            .filter(|&tile| total[tile] >= 12)
            .max_by_key(|&tile| worst_step[tile] * 100 / total[tile].max(1));
        let (share, tile) = popped
            .map(|tile| (worst_step[tile] * 100 / total[tile].max(1), tile))
            .unwrap_or((0, 0));
        assert!(
            share <= 40,
            "{what}: the patch at {},{} did {share}% of everything it ever did in one \
                 frame -                  something appeared or disappeared in one frame",
            (tile % tiles.0) * TILE.0,
            (tile / tiles.0) * TILE.1,
        );
    }

    /// A thing that travels leaves its place. The name and the icon are cut
    /// out of the room and flown to the header, so from the first frame on
    /// the only ones on the panel are the ones in flight: no ghost of the
    /// same name may be left fading in the row they came from.
    pub(crate) fn ghosts(
        room: &[u32],
        screen: &[u32],
        panel_w: usize,
        panel_h: usize,
        plan: LiftPlan,
        what: &str,
    ) {
        let mut art = crate::lift_art(room, screen, panel_w, panel_h, plan);
        let content = lift_content(room, panel_w, panel_h);
        let screen_content = lift_content(screen, panel_w, panel_h);
        for step in 1..=FRAMES {
            let t = step as f32 / FRAMES as f32;
            let mut pixels = vec![0u32; panel_w * panel_h];
            lift_frame(
                Surface {
                    pixels: &mut pixels,
                    stride: panel_w,
                    width: panel_w,
                    height: panel_h,
                },
                (screen, room),
                plan,
                &mut art,
                (&content, &screen_content),
                Shown::Arriving,
                Frame::at(t),
            );
            let flying = lift_pieces(plan, t);
            for (name, traveller) in ["name", "icon"].iter().zip(plan.travellers.iter()) {
                let Some(was) = traveller.map(|fly| fly.from) else {
                    continue;
                };
                for y in was.y..was.y + was.h {
                    for x in was.x..was.x + was.w {
                        // Wherever anything is in flight does not count:
                        // the name and the icon cross each other's places
                        // on their way out of the row.
                        if flying
                            .iter()
                            .flatten()
                            .any(|at| x >= at.x && x < at.x + at.w && y >= at.y && y < at.y + at.h)
                        {
                            continue;
                        }
                        let p = pixels[y as usize * panel_w + x as usize];
                        let bright = [0, 8, 16]
                            .iter()
                            .map(|s| (p >> s) & 0xff)
                            .max()
                            .unwrap_or(0);
                        assert!(
                            bright <= 0x50,
                            "{what}: a ghost of the {name} is still at {x},{y} at {t:.2} \
                                 while the real one has moved away"
                        );
                    }
                }
            }
        }
    }

    /// A traveller lands on itself. The name and the icon are cut out of
    /// the room and flown to the screen's own, so the screen has to draw
    /// them the same way in the same place: then the hand-over is nothing
    /// at all rather than two drawings swapping, which is what made the
    /// icon look like it flipped at the end.
    pub(crate) fn lands(
        room: &[u32],
        screen: &[u32],
        panel_w: usize,
        panel_h: usize,
        plan: LiftPlan,
        what: &str,
    ) {
        for (name, traveller) in ["name", "icon"].iter().zip(plan.travellers.iter()) {
            let Some(&Traveller { from, to, .. }) = traveller.as_ref() else {
                continue;
            };
            // Within a pixel: text is laid out to sub-pixel positions and
            // the two boxes are reached by different arithmetic, so the
            // glyphs can sit a pixel apart. What this is for is a
            // traveller landing on a *different drawing*, which no
            // offset puts right.
            // The mean difference over the rectangle, at the best of the
            // nine offsets within a pixel. A traveller that lands on a
            // *different drawing* - another fill, another glyph - differs
            // everywhere and scores high; one that lands half a pixel out,
            // which is all a layout's arithmetic can promise, differs only
            // along its edges and scores low.
            let miss = |dx: i32, dy: i32| {
                let (mut sum, mut n) = (0usize, 0usize);
                for row in 0..from.h.min(to.h) {
                    for col in 0..from.w.min(to.w) {
                        let (bx, by) = (to.x + col + dx, to.y + row + dy);
                        if bx < 0 || by < 0 || bx >= panel_w as i32 || by >= panel_h as i32 {
                            continue;
                        }
                        let a = room[(from.y + row) as usize * panel_w + (from.x + col) as usize];
                        // The card the sprite was cut against is not put
                        // down, so it is not part of the landing either.
                        if a == LIFT_SURFACE {
                            continue;
                        }
                        let b = screen[by as usize * panel_w + bx as usize];
                        let d = [0, 8, 16]
                            .iter()
                            .map(|s| (((a >> s) & 0xff) as i32 - ((b >> s) & 0xff) as i32).abs())
                            .max()
                            .unwrap_or(0);
                        sum += d as usize;
                        n += 1;
                    }
                }
                sum / n.max(1)
            };
            let best = (-1..=1)
                .flat_map(|dy| (-1..=1).map(move |dx| (dx, dy)))
                .map(|(dx, dy)| miss(dx, dy))
                .min()
                .unwrap_or(usize::MAX);
            assert!(
                best <= 15,
                "{what}: the {name} does not land on itself - {best} a channel out at the \
                     best offset, so the two ends are not the same drawing"
            );
        }
    }

    /// Nothing inside an arriving card is stronger than the card. A card
    /// is blended over the frame at the alpha it has reached, so while
    /// that alpha is low no pixel under it may have moved far from what it
    /// would have been without the card at all - the level's track and the
    /// colour marker used to be painted straight into the frame at their
    /// own strength, and cut holes in the room's rows.
    pub(crate) fn stronger(
        room: &[u32],
        screen: &[u32],
        panel_w: usize,
        panel_h: usize,
        plan: LiftPlan,
        what: &str,
    ) {
        let compose = |plan: LiftPlan, t: f32| {
            let mut art = crate::lift_art(room, screen, panel_w, panel_h, plan);
            let content = lift_content(room, panel_w, panel_h);
            let screen_content = lift_content(screen, panel_w, panel_h);
            let mut pixels = vec![0u32; panel_w * panel_h];
            lift_frame(
                Surface {
                    pixels: &mut pixels,
                    stride: panel_w,
                    width: panel_w,
                    height: panel_h,
                },
                (screen, room),
                plan,
                &mut art,
                (&content, &screen_content),
                Shown::Arriving,
                Frame::at(t),
            );
            pixels
        };
        for step in 1..=FRAMES {
            let t = step as f32 / FRAMES as f32;
            for (which, card, alpha) in lift_cards(plan, t) {
                if card.w <= 0 || alpha > 128 {
                    continue;
                }
                // The same frame with that card not there at all.
                let mut missing = plan;
                missing.pieces[which] = None;
                let (with, without) = (compose(plan, t), compose(missing, t));
                let bound = (alpha * 255 / 256) as i32 + 8;
                for y in card.y..(card.y + card.h + 16).min(panel_h as i32) {
                    for x in card.x..card.x + card.w {
                        let i = y as usize * panel_w + x as usize;
                        let (a, b) = (with[i], without[i]);
                        for shift in [0, 8, 16] {
                            let d =
                                (((a >> shift) & 0xff) as i32 - ((b >> shift) & 0xff) as i32).abs();
                            assert!(
                                d <= bound,
                                "{what}: card {which} is only {alpha}/256 in at {t:.2}, \
                                     but {x},{y} moved {d} - something inside it was drawn at \
                                     its own strength"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_opening_is_the_lift_unless_the_switch_says_otherwise() {
        // The shape the owner chose: a screen that has a plan lifts out of
        // its row, and one that has none keeps its slide whatever this says.
        assert_eq!(Transition::parse(""), Transition::default());
        assert_eq!(Transition::default().opening, Opening::Lift);
        assert_eq!(Transition::default().time, LIFT);
        assert_eq!(LIFT, Duration::from_millis(400));
        let curtain = Transition::parse("curtain 220\n");
        assert_eq!(curtain.opening, Opening::Curtain);
        assert_eq!(curtain.time, Duration::from_millis(220));
        // Either part alone, in either order, and nonsense changes nothing.
        assert_eq!(Transition::parse("curtain").time, IRIS);
        assert_eq!(Transition::parse("iris").opening, Opening::Iris);
        assert_eq!(Transition::parse("iris").time, IRIS);
        assert_eq!(
            Transition::parse("300 iris").time,
            Duration::from_millis(300)
        );
        assert_eq!(Transition::parse("sideways fast"), Transition::default());
        // A time nobody could want is brought back into a range somebody could.
        assert_eq!(Transition::parse("5").time, Duration::from_millis(100));
        assert_eq!(Transition::parse("99999").time, Duration::from_millis(1000));
        // The lift, named or not, and it takes a time like the others.
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

    /// The crossing the lift is built on, on buffers a few pixels across.
    #[test]
    fn a_blend_is_the_mean_of_its_ends_and_a_flat_run_needs_none() {
        // A row that is all one colour fades to one colour, so the
        // compositor fills it rather than blending it: the two have to agree.
        let flat_row: Vec<u32> = std::iter::repeat_n(0xff20_4060u32, W)
            .chain(std::iter::repeat_n(LIFT_BG, W))
            .collect();
        let lines = lift_content(&flat_row, W, 2);
        assert_eq!(
            lines[0],
            Line {
                from: 0,
                to: W as u32,
                flat: Some(0xff20_4060)
            }
        );
        assert_eq!(lines[1], Line::default());
        let mut blended = [0u32; 4];
        blend_flat(&mut blended, &[0xff20_4060; 4], LIFT_BG, 128);
        assert_eq!(blended[0], mix(LIFT_BG, 0xff20_4060, 128));

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
        let win = |x, y, w, h| Window { x, y, w, h, r: 0 };
        // The same shape a real screen hands over, in miniature: a header
        // with a name and an icon flying into it, one bar card, and a footer.
        let lift = LiftPlan::out_of("test", row)
            .carrying(
                (win(4, 6, 6, 3), win(1, 1, 6, 3)),
                (win(2, 6, 2, 2), win(7, 5, 2, 2)),
            )
            .header(W as i32, 5, 7)
            .piece(Piece {
                rect: win(1, 8, 6, 6),
                window: LIFT_CARDS_IN,
                fade: LIFT_CARDS_FADE,
                kind: Arriving::Card {
                    rise: LIFT_BARS_DROP,
                    track: win(2, 9, 4, 4),
                    fill_h: 2,
                    marker_y: -1,
                    marker_h: 1,
                    grow: LIFT_GROW,
                },
            })
            .footer(W as i32, H as i32, 14);
        let at = |shown, t: f32| {
            let mut pixels = vec![0u32; W * H];
            // The page being arrived at is the screen on the way in and the
            // room on the way out; the panel hands them over the same way.
            let pages = match shown {
                Shown::Arriving => (&screen[..], &room[..]),
                Shown::Leaving => (&room[..], &screen[..]),
            };
            // The name and the icon are always cut from the room, whichever
            // way the transition is going, which is what `lift` does.
            let mut art = crate::lift_art(&room, &screen, W, H, lift);
            let content = lift_content(&room, W, H);
            let screen_content = lift_content(&screen, W, H);
            lift_frame(
                Surface {
                    pixels: &mut pixels,
                    stride: W,
                    width: W,
                    height: H,
                },
                pages,
                lift,
                &mut art,
                (&content, &screen_content),
                shown,
                Frame::at(t),
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

    /// A composed frame reaches the panel in one pass, every pixel written
    /// exactly once and the framebuffer's own stride respected.
    ///
    /// This is what makes a transition that paints in several passes safe to
    /// show: the lift flattens a band and puts its pieces back afterwards, and
    /// the panel is scanned out continuously with nothing flipping buffers, so
    /// painting that in place is seen half done - a dark bar walking up the
    /// screen, which is what the owner saw.
    #[test]
    fn a_frame_reaches_the_panel_in_one_pass() {
        // A map wider than the panel, as this device's framebuffer is.
        const STRIDE: usize = W + 3;
        let composed: Vec<u32> = (0..W * H).map(|i| 1000 + i as u32).collect();
        let mut map = vec![0u32; STRIDE * H];
        assert_eq!(present(&mut map, STRIDE, W, H, &composed, None), H);
        for y in 0..H {
            assert_eq!(
                &map[y * STRIDE..y * STRIDE + W],
                &composed[y * W..(y + 1) * W],
                "row {y} did not arrive"
            );
            // The stride's own padding is not the panel and is left alone.
            assert!(map[y * STRIDE + W..(y + 1) * STRIDE]
                .iter()
                .all(|p| *p == 0));
        }
        // Writing it again over a sentinel leaves none of the sentinel behind:
        // every pixel of the panel is covered by exactly one run.
        let mut map = vec![u32::MAX; STRIDE * H];
        present(&mut map, STRIDE, W, H, &composed, None);
        assert_eq!(
            map.iter().filter(|p| **p == u32::MAX).count(),
            (STRIDE - W) * H
        );

        // Only the rows the compose wrote. The rest are on the glass already,
        // and a row nobody touched is not written at all - which is the whole
        // saving, and also what the one-pass rule allows: at most one write a
        // pixel, still top to bottom.
        let mut map = vec![u32::MAX; STRIDE * H];
        let mut dirty = vec![false; H];
        dirty[2] = true;
        dirty[H - 1] = true;
        assert_eq!(present(&mut map, STRIDE, W, H, &composed, Some(&dirty)), 2);
        for y in 0..H {
            let sent = y == 2 || y == H - 1;
            assert_eq!(
                map[y * STRIDE] == composed[y * W],
                sent,
                "row {y} was {} sent",
                if sent { "not" } else { "" }
            );
        }
        // A frame that changed nothing sends nothing at all.
        let none = vec![false; H];
        assert_eq!(present(&mut map, STRIDE, W, H, &composed, Some(&none)), 0);
    }

    /// A lift leaves the panel holding page B exactly, however few rows its
    /// last frames sent.
    ///
    /// The rows that are skipped are skipped because they already hold what
    /// the frame would have written. If that were ever untrue the panel would
    /// keep a stale band for the rest of the transition, so the end is the
    /// place to check it: compose the whole run into one buffer, sending only
    /// what each frame marked, and the map has to be the page.
    #[test]
    fn what_reaches_the_panel_over_a_whole_lift_is_the_page() {
        // Both ways round. A close runs the same plan backwards, so the
        // question "can this row differ from the one on the glass" has to be
        // asked at the point the frame is actually drawn at, not at the clock.
        for shown in [Shown::Arriving, Shown::Leaving] {
            whole_lift(shown);
        }
    }

    fn whole_lift(shown: Shown) {
        const STRIDE: usize = W + 3;
        let room: Vec<u32> = (0..W * H).map(|i| 0xff00_0000 | i as u32).collect();
        let screen: Vec<u32> = (0..W * H).map(|i| 0xff10_0000 | i as u32).collect();
        let row = Window {
            x: 2,
            y: 6,
            w: 12,
            h: 3,
            r: 0,
        };
        let win = |x, y, w, h| Window { x, y, w, h, r: 0 };
        let plan = LiftPlan::out_of("test", row)
            .carrying(
                (win(4, 6, 6, 3), win(1, 1, 6, 3)),
                (win(2, 6, 2, 2), win(7, 5, 2, 2)),
            )
            .header(W as i32, 5, 7)
            .footer(W as i32, H as i32, 14);
        let pages = match shown {
            Shown::Arriving => (&screen[..], &room[..]),
            Shown::Leaving => (&room[..], &screen[..]),
        };
        // Whichever way it is going, the page it ends on is the one arriving.
        let ends_on = match shown {
            Shown::Arriving => &screen,
            Shown::Leaving => &room,
        };
        let mut art = crate::lift_art(&room, &screen, W, H, plan);
        let content = lift_content(&room, W, H);
        let screen_content = lift_content(&screen, W, H);
        let mut back = vec![0u32; W * H];
        let mut map = vec![0u32; STRIDE * H];
        let mut dirty = vec![false; H];
        let mut previous = None;
        let mut sent = 0;
        for step in 0..=checks::FRAMES {
            let t = step as f32 / checks::FRAMES as f32;
            let p = through(shown, t);
            changed_rows(&plan, H, p, previous, &mut dirty);
            lift_frame(
                Surface {
                    pixels: &mut back,
                    stride: W,
                    width: W,
                    height: H,
                },
                pages,
                plan,
                &mut art,
                (&content, &screen_content),
                shown,
                Frame {
                    t,
                    dirty: &mut dirty,
                },
            );
            previous = Some(p);
            sent += present(&mut map, STRIDE, W, H, &back, Some(&dirty));
            // The same frame composed from nothing, which is what skipping
            // work has to be indistinguishable from. Not only at the end:
            // every frame the panel shows has to be the frame it would have
            // shown if nothing had been left out.
            let mut fresh = vec![0u32; W * H];
            lift_frame(
                Surface {
                    pixels: &mut fresh,
                    stride: W,
                    width: W,
                    height: H,
                },
                pages,
                plan,
                &mut crate::lift_art(&room, &screen, W, H, plan),
                (&content, &screen_content),
                shown,
                Frame::at(t),
            );
            for y in 0..H {
                assert_eq!(
                    &map[y * STRIDE..y * STRIDE + W],
                    &fresh[y * W..(y + 1) * W],
                    "{shown:?} at {t:.2} row {y} on the panel is not what a frame drawn from \
                     nothing would have put there"
                );
            }
        }
        for y in 0..H {
            assert_eq!(
                &map[y * STRIDE..y * STRIDE + W],
                &ends_on[y * W..(y + 1) * W],
                "row {y} of the panel is not the page the lift ended on ({shown:?})"
            );
        }
        // And it did skip rows: a run that sends every row every frame has
        // not saved anything.
        assert!(
            sent < H * (checks::FRAMES + 1),
            "every row was sent in every frame, so nothing was skipped"
        );
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
        sprite.put(&mut surface(&mut pixels), (1, 1), None, 256);
        assert_eq!(&pixels[W + 1..W + 4], &page[4 * W + 4..4 * W + 7]);
        assert_eq!(&pixels[2 * W + 1..2 * W + 4], &page[5 * W + 4..5 * W + 7]);
        assert_eq!(pixels[W], 0);
        assert_eq!(pixels[W + 4], 0);
        assert!(pixels[3 * W..].iter().all(|p| *p == 0));

        // Off every edge in turn: what fits arrives, the rest is dropped and
        // nothing wraps round.
        let mut pixels = vec![0u32; W * H];
        sprite.put(&mut surface(&mut pixels), (-1, -1), None, 256);
        assert_eq!(&pixels[0..2], &page[5 * W + 5..5 * W + 7]);
        assert!(pixels[2..W].iter().all(|p| *p == 0));
        let mut pixels = vec![0u32; W * H];
        sprite.put(
            &mut surface(&mut pixels),
            (W as i32 - 1, H as i32 - 1),
            None,
            256,
        );
        assert_eq!(pixels[H * W - 1], page[4 * W + 4]);
        assert!(pixels[..H * W - 1].iter().all(|p| *p == 0));

        // Keyed: the colour the sprite was cut against is not put down, so
        // only the glyphs travel and the card they sat on stays behind.
        let flat = page[4 * W + 5];
        let mut pixels = vec![7u32; W * H];
        sprite.put(&mut surface(&mut pixels), (1, 1), Some(flat), 256);
        // Half of it, over what is already there, is halfway between the two.
        let mut half = vec![0u32; W * H];
        sprite.put(&mut surface(&mut half), (1, 1), None, 128);
        assert_eq!(half[W + 1], (page[4 * W + 4] >> 1) & 0x7f7f_7f7f);
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
            256,
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
