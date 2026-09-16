//! The hardware keypad.
//!
//! Two evdev nodes matter on this device: mt_gpio_kpd and mtk-kpd. Both are
//! read, because which one a given key arrives on is not obvious and a press on
//! the wrong one simply vanishes.
//!
//! Everything read in one batch is decoded, in order, into a queue; `poll`
//! hands back the front of it, one per frame. The queue is what makes two keys
//! pressed inside one frame both count, and it is what keeps a release from
//! being thrown away when a microphone edge arrives beside it - a lost release
//! leaves the key held, and a held key repeats every 70ms until it is pressed
//! and released again.
//!
//! Autorepeat is disabled in the device tree, so holding a key repeats only
//! because this implements it. The debounce is set in the odmdtbo overlay
//! (lowered from 50ms to 8ms) - without that, a rapid double tap arrives as a
//! single press and the UI looks broken.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::os::unix::io::AsRawFd;

use slint::platform::Key;

/// The microphone key, measured on the hardware: it is the only key that
/// arrives as code 61 on mt_gpio_kpd. It is deliberately not in `map_key` -
/// it drives recording, not the focus ring, and it is the one key whose
/// release matters as much as its press.
pub const KEY_MIC: u16 = 61;

/// PMIC side button; the faceplate TV power button reports F2 (60).
pub const KEY_SIDE_POWER: u16 = 116;

/// The menu button, KEY_MENU. Reported as an edge like the microphone, because
/// the home screen opens settings on a *hold* of it and only the host can time
/// a hold. It is not routed into the UI as a key.
pub const KEY_MENU: u16 = 139;

pub const REPEAT_DELAY_MS: u64 = 400;
pub const REPEAT_RATE_MS: u64 = 70;

/// evdev's input_event is 16 bytes on 32-bit ARM: two 4-byte timeval words,
/// then type, code and value.
const EVENT_SIZE: usize = 16;

/// The only event type a keypad says anything with. The EV_SYN that ends every
/// packet, and the EV_MSC scancode that precedes some of them, are read and
/// ignored.
const EV_KEY: u16 = 1;

/// A ceiling on the backlog. The UI takes one press per frame, so a queue this
/// long is already half a second of input the frame loop never caught up with:
/// a key chattering under the 8ms debounce, or a frame that took far too long.
/// Replaying all of it minutes later is worse than skipping ahead. What gets
/// skipped is the oldest key press - never a microphone edge, because a
/// dropped release is a microphone that stays open.
const MAX_QUEUED: usize = 32;

pub struct Keypad {
    files: Vec<File>,
    /// Decoded presses in the order the kernel produced them, across both
    /// nodes. `poll` takes from the front.
    queue: VecDeque<Press>,
    held: u16,
    held_since: u64,
    last_repeat: u64,
}

#[derive(Clone)]
pub struct Press {
    pub code: u16,
    pub released: bool,
    pub key: Option<Key>,
    /// Some(true) when the microphone key went down, Some(false) when it came
    /// back up. Hold to talk, so both edges are events.
    pub mic: Option<bool>,
    /// Some(true)/Some(false) for the menu key's down/up edges. Hold to open
    /// settings, so both edges matter and the host times the hold.
    pub menu: Option<bool>,
    /// Microseconds between the kernel timestamping the event and us seeing it.
    pub latency_us: u64,
    pub repeat: bool,
}

impl Keypad {
    pub fn open() -> Self {
        let files = crate::evdev::open_named(&["mt_gpio_kpd", "mtk-kpd"]);
        Keypad {
            files,
            queue: VecDeque::new(),
            held: 0,
            held_since: 0,
            last_repeat: 0,
        }
    }

    pub fn device_count(&self) -> usize {
        self.files.len()
    }

    /// While the display is off, sleep until a key arrives or the next
    /// one-second service update is due. The kernel wakes poll immediately.
    pub fn wait_for_input(&self) {
        let mut fds: Vec<libc::pollfd> = self
            .files
            .iter()
            .map(|file| libc::pollfd {
                fd: file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        unsafe {
            libc::poll(fds.as_mut_ptr(), fds.len() as _, 1000);
        }
    }

    /// Non-blocking. Returns at most one press per call, which is what the UI
    /// wants - one key per frame - but the rest of the batch is kept rather
    /// than dropped, so a burst arrives over the next frames instead of
    /// vanishing.
    pub fn poll(&mut self) -> Option<Press> {
        // Both clocks are read once for the whole batch: they are syscalls,
        // and the batch spans microseconds.
        let (mono, real) = (now_monotonic_us(), now_realtime_us());
        self.drain(mono, real);
        if let Some(press) = self.queue.pop_front() {
            return Some(press);
        }
        self.repeat(mono)
    }

    /// Read whatever both nodes have and decode all of it. Every event is
    /// decoded even when the first of them is already a press to return: the
    /// held-key state has to come from all of them or a release goes missing.
    fn drain(&mut self, mono: u64, real: u64) {
        let mut buf = [0u8; EVENT_SIZE * 32];
        // Indexed rather than iterated: `iter_mut` would hold a borrow of the
        // files across `feed`, which wants &mut self.
        for i in 0..self.files.len() {
            loop {
                let n = match self.files[i].read(&mut buf) {
                    Ok(n) if n >= EVENT_SIZE => n,
                    // EAGAIN on a node nobody is pressing, which is the usual
                    // answer on both of them.
                    _ => break,
                };
                self.feed(&buf[..n - n % EVENT_SIZE], mono, real);
                // Anything short of a full buffer means the node is empty; a
                // full one may have left more behind.
                if n < buf.len() {
                    break;
                }
            }
        }
    }

    /// One read's worth of raw events, in order.
    fn feed(&mut self, bytes: &[u8], mono: u64, real: u64) {
        for ev in bytes.chunks_exact(EVENT_SIZE) {
            if let Some(press) = self.decode(ev, mono, real) {
                self.push(press);
            }
        }
    }

    /// Queue a decoded press, making room by dropping the oldest key press if
    /// the UI has fallen `MAX_QUEUED` behind. Microphone edges are never the
    /// ones dropped.
    fn push(&mut self, press: Press) {
        if self.queue.len() >= MAX_QUEUED {
            if let Some(at) = self.queue.iter().position(|p| p.mic.is_none()) {
                self.queue.remove(at);
            }
        }
        self.queue.push_back(press);
    }

    /// One raw 16-byte event against the held-key state: the only place that
    /// state changes, and the only place a `Press` is made from hardware.
    fn decode(&mut self, ev: &[u8], mono: u64, real: u64) -> Option<Press> {
        let sec = u32::from_ne_bytes(ev[0..4].try_into().unwrap()) as u64;
        let usec = u32::from_ne_bytes(ev[4..8].try_into().unwrap()) as u64;
        let etype = u16::from_ne_bytes(ev[8..10].try_into().unwrap());
        let code = u16::from_ne_bytes(ev[10..12].try_into().unwrap());
        let value = i32::from_ne_bytes(ev[12..16].try_into().unwrap());
        if etype != EV_KEY {
            return None;
        }
        // evdev stamps against CLOCK_REALTIME, so latency has to be measured
        // on the same clock: subtracting an epoch from a monotonic uptime
        // saturates to zero and reads as "no latency".
        let latency_us = real.saturating_sub(sec * 1_000_000 + usec);
        match value {
            0 => {
                if code == self.held {
                    self.held = 0;
                }
                if code == KEY_MIC {
                    return Some(Press {
                        code,
                        released: true,
                        key: None,
                        mic: Some(false),
                        menu: None,
                        latency_us,
                        repeat: false,
                    });
                }
                if code == KEY_MENU {
                    return Some(Press {
                        code,
                        released: true,
                        key: None,
                        mic: None,
                        menu: Some(false),
                        latency_us,
                        repeat: false,
                    });
                }
                Some(Press {
                    code,
                    released: true,
                    key: None,
                    mic: None,
                    menu: None,
                    latency_us,
                    repeat: false,
                })
            }
            1 => {
                if code == KEY_MIC {
                    // Not held for repeat: a key you hold down to talk must not
                    // also be a key that repeats.
                    return Some(Press {
                        code,
                        released: false,
                        key: None,
                        mic: Some(true),
                        menu: None,
                        latency_us,
                        repeat: false,
                    });
                }
                if code == KEY_MENU {
                    // The home screen opens settings on a hold; the host times
                    // it from this down edge and the up edge above. Not held
                    // for repeat, for the same reason as the mic key.
                    return Some(Press {
                        code,
                        released: false,
                        key: None,
                        mic: None,
                        menu: Some(true),
                        latency_us,
                        repeat: false,
                    });
                }
                // A key this build does not know still counts as a press:
                // any button on a dark panel must wake it, and the idle timer
                // must not run while someone is pressing keys the UI happens
                // not to use. It carries no `key`, so nothing is dispatched to
                // the UI, and it is not held, so it does not repeat a nameless
                // press every 70ms the way a held mapped key would. The code
                // is logged once so a button that does nothing else can be
                // found and mapped.
                let Some(key) = map_key(code) else {
                    println!("couch-gui: unmapped key code {code}");
                    return Some(Press {
                        code,
                        released: false,
                        key: None,
                        mic: None,
                        menu: None,
                        latency_us,
                        repeat: false,
                    });
                };
                self.held = if one_shot(code) { 0 } else { code };
                self.held_since = mono;
                self.last_repeat = 0;
                Some(Press {
                    code,
                    released: false,
                    key: Some(key),
                    mic: None,
                    menu: None,
                    latency_us,
                    repeat: false,
                })
            }
            // 2 is the kernel's own autorepeat. The device tree turns it off;
            // if it ever arrives, this one is not going to act on it twice.
            _ => None,
        }
    }

    /// The held key, once it is due. Only reached when the batch produced
    /// nothing, so a repeat never gets in front of a real press.
    fn repeat(&mut self, now: u64) -> Option<Press> {
        if self.held == 0 {
            return None;
        }
        let due = if self.last_repeat == 0 {
            self.held_since + REPEAT_DELAY_MS * 1000
        } else {
            self.last_repeat + REPEAT_RATE_MS * 1000
        };
        if now < due {
            return None;
        }
        self.last_repeat = now;
        Some(Press {
            code: self.held,
            released: false,
            key: map_key(self.held),
            mic: None,
            menu: None,
            latency_us: 0,
            repeat: true,
        })
    }
}

// Measured HA100 legends: Power=F2, Home=F1, R/G/B/Y=F8/F9/F10/F11.
// Reserve F13–F18 for TV actions; never repeat power, mute, home or colors.
fn one_shot(code: u16) -> bool {
    !matches!(
        code,
        103 | 108 | 105 | 106 | 104 | 109 | 402 | 403 | 115 | 114
    )
}
fn map_key(code: u16) -> Option<Key> {
    match code {
        103 => Some(Key::UpArrow),    // KEY_UP
        108 => Some(Key::DownArrow),  // KEY_DOWN
        105 => Some(Key::LeftArrow),  // KEY_LEFT
        106 => Some(Key::RightArrow), // KEY_RIGHT
        // 353 is KEY_SELECT, which is what this remote's OK button actually
        // sends - measured, after it silently did nothing for a while. 352 is
        // KEY_OK, its neighbour in the same block, mapped on the same grounds
        // that 28 and 96 both are.
        28 | 96 | 352 | 353 => Some(Key::Return),
        1 | 158 => Some(Key::Escape), // KEY_ESC / KEY_BACK
        // Slint has no volume key variants; reserve F23/F24 as internal
        // volume-up/down tokens. They follow normal hold-to-repeat handling.
        // The remote advertises page keys; also accept standard channel codes.
        104 | 402 => Some(Key::F21), // PAGEUP / CHANNELUP
        109 | 403 => Some(Key::F22), // PAGEDOWN / CHANNELDOWN
        115 => Some(Key::F23),       // KEY_VOLUMEUP
        114 => Some(Key::F24),       // KEY_VOLUMEDOWN
        60 => Some(Key::F13),        // measured Power / standard KEY_POWER
        113 => Some(Key::F14),       // mute
        66 | 398 => Some(Key::F15),  // red
        67 | 399 => Some(Key::F16),  // green
        68 | 401 => Some(Key::F17),  // blue
        87 | 400 => Some(Key::F18),  // yellow
        62 => Some(Key::F3),
        63 => Some(Key::F4),
        64 => Some(Key::F5),
        65 => Some(Key::F6),
        59 | 172 => Some(Key::Home), // KEY_HOMEPAGE
        _ => None,
    }
}

pub fn now_monotonic_us() -> u64 {
    clock_us(libc::CLOCK_MONOTONIC)
}

pub fn now_realtime_us() -> u64 {
    clock_us(libc::CLOCK_REALTIME)
}

fn clock_us(which: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(which, &mut ts) };
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A raw event laid out the way the kernel does it: native endian, the
    /// timestamp in the first two words.
    fn raw(etype: u16, code: u16, value: i32, stamp_us: u64) -> [u8; EVENT_SIZE] {
        let mut b = [0u8; EVENT_SIZE];
        b[0..4].copy_from_slice(&((stamp_us / 1_000_000) as u32).to_ne_bytes());
        b[4..8].copy_from_slice(&((stamp_us % 1_000_000) as u32).to_ne_bytes());
        b[8..10].copy_from_slice(&etype.to_ne_bytes());
        b[10..12].copy_from_slice(&code.to_ne_bytes());
        b[12..16].copy_from_slice(&value.to_ne_bytes());
        b
    }

    #[test]
    fn side_power_is_not_a_device_command_and_never_repeats() {
        let mut pad = blank();
        let press = pad.decode(&down(KEY_SIDE_POWER), 1, 1).unwrap();
        assert_eq!(press.code, KEY_SIDE_POWER);
        assert!(press.key.is_none());
        assert!(pad.repeat(1_000_000).is_none());
        assert!(pad
            .decode(&raw(EV_KEY, KEY_SIDE_POWER, 2, 0), 2, 2)
            .is_none());
        assert!(pad.decode(&up(KEY_SIDE_POWER), 3, 3).unwrap().released);
        assert_eq!(map_key(60), Some(Key::F13));
        assert_eq!(
            couch_model::buttons::Button::from_evdev(KEY_SIDE_POWER),
            None
        );
        assert_eq!(
            couch_model::buttons::Button::from_evdev(60),
            Some(couch_model::buttons::Button::Power)
        );
    }

    fn down(code: u16) -> [u8; EVENT_SIZE] {
        raw(EV_KEY, code, 1, 0)
    }

    fn up(code: u16) -> [u8; EVENT_SIZE] {
        raw(EV_KEY, code, 0, 0)
    }

    /// The EV_SYN that ends every packet. Present in every real batch, so the
    /// tests carry it too.
    fn syn() -> [u8; EVENT_SIZE] {
        raw(0, 0, 0, 0)
    }

    fn blank() -> Keypad {
        Keypad {
            files: Vec::new(),
            queue: VecDeque::new(),
            held: 0,
            held_since: 0,
            last_repeat: 0,
        }
    }

    /// Drives the real decoder over one read's worth of bytes, with no device,
    /// and reports the queue it produced. `char::from` because that is what
    /// the UI does with a key, and it needs no traits on `Key`.
    fn batch(
        k: &mut Keypad,
        events: &[[u8; EVENT_SIZE]],
        mono: u64,
    ) -> Vec<(Option<char>, Option<bool>, bool)> {
        k.feed(&events.concat(), mono, 0);
        std::iter::from_fn(|| k.queue.pop_front())
            .filter(|p| !p.released || p.mic.is_some())
            .map(|p| (p.key.map(char::from), p.mic, p.repeat))
            .collect()
    }

    fn key(k: Key) -> (Option<char>, Option<bool>, bool) {
        (Some(char::from(k)), None, false)
    }

    /// The bug this guards: the microphone edge used to return from the middle
    /// of the batch, and everything decoded beside it went in the bin.
    #[test]
    fn channel_keys_accept_page_and_channel_codes() {
        for code in [104, 402] {
            assert_eq!(map_key(code).map(char::from), Some(char::from(Key::F21)));
        }
        for code in [109, 403] {
            assert_eq!(map_key(code).map(char::from), Some(char::from(Key::F22)));
        }
    }

    #[test]
    fn measured_tv_buttons_fire_once_even_when_held() {
        for (code, mapped) in [
            (60, Key::F13),
            (59, Key::Home),
            (113, Key::F14),
            (66, Key::F15),
            (67, Key::F16),
            (68, Key::F17),
            (87, Key::F18),
        ] {
            let mut k = blank();
            assert_eq!(batch(&mut k, &[down(code), syn()], 1), vec![key(mapped)]);
            assert!(k.repeat(2_000_000).is_none());
            assert!(batch(
                &mut k,
                &[raw(EV_KEY, code, 2, 0), up(code), syn()],
                2_100_000
            )
            .is_empty());
        }
        let mut k = blank();
        batch(&mut k, &[down(115), syn()], 1);
        assert!(k.repeat(2_000_000).is_some());
    }
    #[test]
    fn volume_keys_map_to_repeatable_brightness_inputs() {
        assert_eq!(map_key(115).map(char::from), Some(char::from(Key::F23)));
        assert_eq!(map_key(114).map(char::from), Some(char::from(Key::F24)));
    }

    #[test]
    fn a_key_and_a_mic_edge_in_one_batch_both_arrive_in_order() {
        let mut k = blank();
        let out = batch(&mut k, &[down(103), syn(), down(KEY_MIC), syn()], 1_000);
        assert_eq!(out, vec![key(Key::UpArrow), (None, Some(true), false)]);
    }

    #[test]
    fn both_mic_edges_survive_one_batch() {
        let mut k = blank();
        let out = batch(&mut k, &[down(KEY_MIC), syn(), up(KEY_MIC), syn()], 1_000);
        assert_eq!(
            out,
            vec![(None, Some(true), false), (None, Some(false), false),]
        );
        assert_eq!(k.held, 0, "the microphone key is never held for repeat");
    }

    /// A release dropped from a batch is what wedges the key down and starts
    /// the 70ms repeat that never stops.
    #[test]
    fn a_press_and_its_release_in_one_batch_leave_nothing_held() {
        let mut k = blank();
        let out = batch(
            &mut k,
            &[down(103), syn(), up(103), syn(), down(KEY_MIC), syn()],
            1_000,
        );
        assert_eq!(out, vec![key(Key::UpArrow), (None, Some(true), false)]);
        assert_eq!(k.held, 0);
        assert!(
            k.repeat(u64::MAX).is_none(),
            "nothing is held, so nothing repeats"
        );
    }

    /// One per call, and the rest kept: two keys inside one frame are two
    /// presses, on this frame and the next.
    #[test]
    fn poll_hands_back_one_press_per_call() {
        let mut k = blank();
        // Held since now, so the repeat delay cannot have elapsed by the time
        // the queue runs out and `poll` looks at the clock.
        k.feed(
            &[down(103), syn(), down(108), syn()].concat(),
            now_monotonic_us(),
            0,
        );
        assert_eq!(
            k.poll().and_then(|p| p.key).map(char::from),
            Some(char::from(Key::UpArrow))
        );
        assert_eq!(
            k.poll().and_then(|p| p.key).map(char::from),
            Some(char::from(Key::DownArrow))
        );
        assert!(k.poll().is_none());
    }

    #[test]
    fn a_release_only_clears_the_key_it_belongs_to() {
        let mut k = blank();
        batch(&mut k, &[down(103), down(108), up(103)], 1_000);
        assert_eq!(
            k.held, 108,
            "releasing the older key must not unhold the newer"
        );
    }

    #[test]
    fn a_held_key_waits_the_delay_then_repeats_at_the_rate() {
        let mut k = blank();
        let t0 = 10_000_000;
        assert_eq!(
            batch(&mut k, &[down(103), syn()], t0),
            vec![key(Key::UpArrow)]
        );

        let delay = REPEAT_DELAY_MS * 1000;
        let rate = REPEAT_RATE_MS * 1000;
        assert!(k.repeat(t0 + delay - 1).is_none());
        let first = k.repeat(t0 + delay).expect("due at the delay");
        assert_eq!(first.key.map(char::from), Some(char::from(Key::UpArrow)));
        assert!(first.repeat);
        assert!(k.repeat(t0 + delay + rate - 1).is_none());
        assert!(k.repeat(t0 + delay + rate).is_some());
    }

    /// A code with no meaning here is logged once and then left alone: held
    /// would make it repeat, and a repeat of nothing is a `Press` carrying no
    /// key several times a second. It still emits one bare press per down, so
    /// an unknown button wakes the panel, but carries no key and is not held.
    #[test]
    fn an_unmapped_code_wakes_but_is_not_held_and_carries_no_key() {
        let mut k = blank();
        let out = batch(&mut k, &[down(999), syn(), up(999), syn()], 1_000);
        assert_eq!(
            out,
            vec![(None, None, false)],
            "one bare press, no key, no mic"
        );
        assert_eq!(k.held, 0);
        assert!(
            k.repeat(u64::MAX).is_none(),
            "nothing held, so nothing repeats"
        );
    }

    /// A key chattering while the frame loop is stuck must not leave a backlog
    /// that replays for the rest of the evening - and the microphone edges
    /// buried in it must still come out, or the microphone stays open.
    #[test]
    fn a_storm_of_presses_is_capped_and_never_costs_a_mic_edge() {
        let mut k = blank();
        let mut storm = vec![down(KEY_MIC), syn()];
        for _ in 0..MAX_QUEUED * 2 {
            storm.extend_from_slice(&[down(103), up(103), syn()]);
        }
        storm.extend_from_slice(&[up(KEY_MIC), syn()]);
        k.feed(&storm.concat(), 1_000, 0);

        assert!(k.queue.len() <= MAX_QUEUED, "{} queued", k.queue.len());
        let mics: Vec<bool> = k.queue.iter().filter_map(|p| p.mic).collect();
        assert_eq!(mics, vec![true, false]);
    }

    /// The stamp is CLOCK_REALTIME; measuring it against a monotonic uptime
    /// saturates to zero and reads as a keypad with no latency at all.
    #[test]
    fn latency_is_measured_against_the_kernel_stamp() {
        let mut k = blank();
        k.feed(&raw(EV_KEY, 103, 1, 4_000_000), 0, 4_002_500);
        assert_eq!(k.queue[0].latency_us, 2_500);
    }
}
