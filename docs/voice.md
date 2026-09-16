# Voice

Hold a button, speak, and get back two things: what was heard, as text, and
what the house said in reply. The transcription is what a search field or the
on-screen keyboard wants; the reply is what the assistant did about it.

> **Nothing here listens unless somebody asks it to.** There is no wake word,
> no open microphone and no path that starts a recording from the network.
> Every recording has a hard cap it cannot exceed. See [Security and
> privacy](#security-and-privacy), which is a requirement of this design and
> not a footnote to it.

Two crates' worth of work in one: `clients/couch-voice/` builds `couch-voice`,
which records and talks to Home Assistant, and `couch-mic`, which only records
and has no network code in it at all.

## On the remote

Wired into `couch-gui` (`ui/couch-gui/src/mic.rs`) as of #133. Hold the
microphone key, or tap it to latch, and the capture streams to the Assist
pipeline of the **first saved Home Assistant connection** (Connections → Home
Assistant in the web UI; its URL and long-lived token) while you speak. Two
uses, chosen by what is on screen when the key goes down:

- **The on-screen keyboard is open:** the run stops at speech-to-text and the
  words are appended to the keyboard's field. Dictating a search beats spelling
  it on a D-pad.
- **Anywhere else:** the run continues to the intent stage. The overlay shows
  what was heard and what the assistant said back, for a few seconds.

The overlay is the contract below made real: red border and dot only while the
device is open, a level meter only while the device is open, and the phase in
words (CONNECTING, LISTENING, THINKING, HEARD, VOICE FAILED). Home Assistant's
voice activity detection ends the stream on 0.7 s of silence; the remote trips
its own stop handle when that happens so a latched key does not keep the
microphone open for a transcript that is already in. With no Home Assistant
connection the key still records to `/tmp/couch-voice.wav` (the microphone
probe) and the overlay says what to configure. An `https://` Home Assistant
URL is refused with a message: the client speaks plain WebSocket only, see
`wss://` below. Nothing is written to disk when a hub is configured.

Not done yet: registering the remote as a Home Assistant device so the
assistant knows which room "the light" is in (`Options.device_id`), choosing a
pipeline other than the preferred one, and resolving a dictated title against
the Kodi library as the accuracy section proposes.

## Is speech-to-text even realistic here? Yes, and not on this device

Two questions that get confused with each other. *Can this remote do speech to
text* is yes. *Can this remote do speech to text by itself* is no, and not
marginally.

### Server-side, through Home Assistant: yes, and it is the intended path

Home Assistant's Assist pipeline exists precisely for this. Its WebSocket API
takes streamed audio and hands back the transcription, the intent result and a
spoken reply, and the work happens on whatever the hub is - which in this house
is not a phone SoC from 2015. The remote's job reduces to: open a capture
device, push 32 kB per second of PCM up a socket, and read events. That is
`clients/couch-voice/`, it is 493 KB of static binary, and it needs nothing on
the device that is not already there.

The device side is cheap in a way worth stating: 16 kHz mono 16-bit is 32 kB/s,
which is 0.26 Mbit/s on a WiFi link that already streams a config UI. There is
no encoding, no resampling and no buffering beyond one hardware period. The
expensive half is somebody else's.

### On-device: no. Not with a model worth using

The MT6580 is four Cortex-A7 cores at about 1.3 GHz with 1 GB of RAM, no NPU,
no usable GPU compute, and NEON that is 64 bits wide. Take the two candidates
seriously and both fail, for different reasons.

**whisper.cpp.** `tiny.en` is 39M parameters, 75 MB as float16 and about 31 MB
quantised. A [Raspberry Pi Zero 2 W - four Cortex-A53 at 1.0 GHz - runs
`tiny.en` at roughly 18 seconds of latency for a short
utterance](https://gist.github.com/Gilzone/f558a6779f742f30cfcb9c83b912a8ff).
Cortex-A7 is an older in-order core with half the NEON datapath width of A53,
so per clock it is meaningfully slower; at 1.3 GHz against 1.0 GHz the two
partly cancel, and the honest estimate for this board is **25-40 seconds for a
three-second command**. Whisper makes that worse rather than better for short
utterances: its encoder always runs over a padded 30-second window, so a
two-word command costs the same as a sentence. Thirty seconds of silence after
letting go of a button is not a voice assistant, it is a bug report.

**Vosk (streaming Kaldi).** Structurally the better fit - it is streaming, so
the work happens *during* the utterance rather than after it, and it is a
fraction of Whisper's arithmetic. The [project's own figures are a ~50 MB small
model needing ~300 MB of RAM at runtime, achieving real-time streaming on a Pi
3](https://alphacephei.com/vosk/models). A Pi 3 is again Cortex-A53; on A7 the
margin over real time disappears and 300 MB of a 937 MB budget is a third of
the device, alongside `couch-gui` and `couch-confd`. And it dies on the
toolchain before any of that matters: Vosk ships `libvosk.so` built against
glibc with a C++ runtime, and Couch's clients are static musl binaries linked
with `rust-lld` and no cross sysroot. Porting Kaldi to armv7 musl is a project,
not a task.

**Verdict.** On-device speech to text on the HA100 is not viable and is not
worth attempting. Even if the CPU were twice as fast, a small model listening
through one analogue microphone across a living room would transcribe badly,
and the failure would be invisible - a wrong word, not an error. The right
answer is the one Home Assistant already built: send the audio to the hub, and
let whoever runs the house decide whether that hub uses a local Whisper add-on
or a cloud service. That decision is theirs, it is made in Home Assistant, and
this client is unaffected by it.

What *is* worth doing on-device, later, is the cheap end: a voice-activity
gate so that pressing the button in a silent room does not open a pipeline at
all, and a level meter so the user can see it is hearing them. Both are
arithmetic on 16-bit samples. The gate is not built; the meter is
(`src/level.rs`).

## What the hardware is

The task said the microphone was unverified. It is still unproven on the
bench - nothing here has touched the device - but it is no longer unknown,
because the shipped kernel and the shipped Android vendor partition both
describe it. From `tools/backup.sh`'s images:

`vendor.img` carries the ALPS build configuration this device was compiled
from, and `vendor/etc/audio_device.xml` repeats part of it:

```
MTK_AUDIO_NUMBER_OF_MIC = 1
MTK_DIGITAL_MIC_SUPPORT = no
MTK_AUDIO_HD_REC_SUPPORT = yes
MTK_AUDIO_SPEAKER_PATH = int_spk_amp
MTK_VOW_SUPPORT = no
MTK_ASR_SUPPORT = no
CUSTOM_KERNEL_SOUND = amp_6323pmic_spk
```

**One microphone, and an analogue one** - `MTK_DIGITAL_MIC_SUPPORT = no` means
a capsule on the MT6350 PMIC codec's AMIC input, not a digital MEMS on a DMIC
clock. `MTK_DUAL_MIC_SUPPORT = yes` also appears, but that flag is on by
default across MediaTek's tree and `NUMBER_OF_MIC` is the specific one.
`MTK_VOW_SUPPORT = no` is worth noticing for a different reason: the hardware
"voice on wakeup" block is not enabled in this build, so there is no
always-listening path on this device to accidentally turn on.

### Which `pcmC0D*c` is the microphone

The card is `mt-snd-card`, MediaTek's mt6580 ASoC machine driver
(`sound/soc/mediatek/mt6580/mt_soc_machine.c` - the vendor's build path leaked
in a panic, so the source layout is known). ALSA numbers PCM devices by their
position in the machine driver's `dai_link` array, and that array's names are
in the shipped kernel image in order:

| dev | dai link | stream | what it is |
|----:|----------|--------|------------|
| 0 | `MultiMedia1` | playback | DL1, the primary output |
| **1** | **`MultiMedia2`** | **capture** | **UL1 - the primary record path** |
| 2 | `Voice_MD1` | both | modem voice call |
| 3 | `HDMI_OUT` | playback | |
| 4 | `ULDLOOPBACK` | both | uplink/downlink loopback |
| 5 | `I2S0OUTPUT` | playback | |
| 6 | `MRGRX` | playback | merge interface (FM/BT) |
| 7 | `MRGRXCAPTURE` | capture | |
| 8 | `I2S0DL1OUTPUT` | playback | |
| 9 | `DEEP_BUFFER_DL_OUTPUT` | playback | |
| 10 | `DL1AWBCAPTURE` | capture | loopback of what was *played* |
| 11 | `Voice_MD1_BT` | playback | |
| 12 | `VOIP_CALL_BT_PLAYBACK` | playback | |
| 13 | `VOIP_CALL_BT_CAPTURE` | capture | |
| 14 | `TDM_Debug_CAPTURE` | capture | |
| 15 | `FM_MRG_TX` | playback | |
| 16 | `MultiMedia3` | capture | UL2, a second record path |
| 17 | `I2S0_AWB_CAPTURE` | capture | |
| 18 | `Voice_MD2` | both | |
| 19 | `Voice_MD2_BT` | playback | |
| 20 | `HP_IMPEDANCE` | playback | |
| 21 | `FM_I2S_RX_Playback` | playback | |
| 22 | `FM_I2S_RX_Capture` | capture | |
| 23 | `MultiMedia_DL2` | playback | |

That is a derivation, not a reading, so it is worth saying how well it holds:
it predicts the direction of every node in the observed `/dev/snd` listing at
devices 0, 1, 2, 4, 10, 13, 14, 16, 17 and 18, and disagrees at the tail (the
device shows `pcmC0D22p/c`, `D23p/c` and a `D24p` this table does not account
for), which suggests the machine driver appends links this list does not have.

**So: `hw:0,1` is the microphone, and `hw:0,16` is the fallback.** `couch-mic`
does not take that on trust - it prints each device's own `snd_pcm_info.id`,
which ASoC fills in from the DAI stream name, so the answer comes back as
`MultiMedia1_Capture` in the device's own words.

Do not be fooled by `DL1AWBCAPTURE` at device 10. AWB is "audio write back":
it captures what was *played*, so it will happily produce a signal that has
nothing to do with the microphone.

### The mixer is not optional

This is the trap. MediaTek's mt6580 driver barely uses DAPM. The analogue path
is switched by kcontrol `put` handlers that call the PMIC codec directly, so
**the PCM device can be open, clocked and delivering perfectly-timed frames
while the ADC in front of it is powered down**. The frames are real. They are
silent. It looks exactly like a microphone that is not connected.

Android's own audio HAL brings the path up through these controls, which are
in both the kernel and `vendor/lib/libaudio.primary.so`:

```
AUD_CLK_BUF_Switch          the analogue clock buffer
Audio_MicSource1_Setting    which physical input the preamp listens to
Audio_MIC1_Mode_Select      ACCMODE / DCCMODE / DCCECMDIFFMODE / DCCECMSINGLEMODE
Audio_Preamp1_Switch        the preamp
Audio_ADC_1_Switch          the converter
Audio_PGA1_Setting          gain, in dB steps
Audio_ADC_1_Sel             IN_ADC1 / IN_ADC2 / IN_ADC3 / Preamp
```

`clients/couch-voice/src/ctl.rs` implements the ALSA control interface and
carries that list as `MTK_AMIC_ROUTE`, in the order the HAL uses it. It is
explicitly a *starting point*: the names are facts, the values are educated
guesses, and `couch-mic controls` prints what each control will actually
accept so the guessing stops. Nothing applies a route unless asked.

## Talking to ALSA without alsa-lib

Every ALSA crate on crates.io binds `libasound`. Couch's clients are static
musl binaries linked with `rust-lld` and no cross-gcc, no Docker and no
sysroot - the constraint that already cost this project a day over
`yeslogic-fontconfig-sys`. So the choice was:

| | cost |
|---|---|
| **link alsa-lib** | a cross sysroot, a C toolchain, and alsa-lib's plugin/config machinery on a device with no `/usr/share/alsa` |
| **bundle tinyalsa or busybox** | a second C build, a second artefact to deploy and keep in step, and parsing another program's stdout to find out what happened |
| **speak to the kernel directly** | ~700 lines of transcribed ABI, and the risk of transcribing it wrong |

The third, because the second and third are the same work: `tinyalsa` *is* the
ioctl sequence, in 1500 lines of C, and wrapping it in a subprocess adds a
build, a deploy and a parser without removing a single ioctl. The device
already has no `/usr/share/alsa`, so alsa-lib's `hw:` plugin would be the only
part of it we could use anyway. `alsa-utils` from `apk` remains a perfectly
good independent cross-check - `arecord -D hw:0,1 -f S16_LE -r 16000 -c 1` -
and is worth installing if a probe result ever looks suspicious.

The risk in the third option is real and is dealt with directly. A wrong ioctl
number or a struct field at the wrong offset does not fail to compile: it
produces `ENOTTY` or `EFAULT` from a driver that is working perfectly, which
is the most expensive kind of wrong there is. So `src/abi.rs` is checked
against the kernel's own header:

```sh
tools/alsa-abi-check.sh
```

That compiles every size, offset and ioctl number as C `_Static_assert`s
against `sound/asound.h` for `armv7a-linux-androideabi` - the same 32-bit ARM
EABI the device's musl uses, where `long` is 4 bytes and `long long` is 8-byte
aligned. Nothing runs; if it compiles, the assertions hold. The device's kernel
is 3.18, whose `asound.h` defines all five of these structs identically to the
header that check uses. The one struct that *did* change shape across kernel
versions, `snd_pcm_status`, is deliberately not used - which is why overruns
are detected by `readi` returning `EPIPE` rather than by asking for status.

Two deliberate departures from what alsa-lib would do:

* **The device is opened `O_NONBLOCK` and every read is fronted by `poll` with
  a deadline.** A capture device whose DMA never starts - exactly what a
  powered-down analogue path produces - blocks forever in a blocking `readi`,
  and a probe that hangs tells you nothing. With a deadline it says "this
  device produced no audio in four seconds", which is an answer.
* **Overruns are counted, not raised.** One on a busy device is normal; a
  hundred means the period is too small. `Capture::overruns()` reports them and
  the CLI prints the count.

## Capture format

16-bit signed, mono, 16 kHz. That is what Home Assistant's speech-to-text stage
runs at (`SAMPLE_RATE = 16000`, `SAMPLE_WIDTH = 2`, `SAMPLE_CHANNELS = 1` in
`assist_pipeline/const.py`) and asking the hardware for it means nothing has to
be resampled anywhere.

Anything else is handled rather than refused:

* **A different rate.** `input.sample_rate` tells Home Assistant what is
  coming, and it resamples with `audioop.ratecv` if it is not 16000. So
  `--rate 48000` is legitimate if the codec insists; it costs three times the
  bytes on a link that has them to spare.
* **A different sample size.** The hardware settling on `S32_LE` is converted
  to `S16_LE` on the device. `S24_3LE` and the rest are refused by name, with
  the list of what the device *does* accept.
* **More than one channel.** Channels are averaged rather than dropped, so a
  capsule wired to the second channel of a stereo capture still produces audio.

Periods are 1024 frames (64 ms) with a four-period buffer: a 256 ms ring, so
192 ms of slack against scheduling on a four-core A7, for 8 KB of kernel memory
and one 2 KB buffer in userspace. One period is also one WebSocket binary frame
and one level-meter update, so a UI driven off it refreshes about fifteen times
a second.

## The Home Assistant pipeline, message by message

Plain `ws://` on the LAN; port 8123; endpoint `/api/websocket`.

```
remote                                        Home Assistant
  |  <- {"type":"auth_required","ha_version":"2026.9.0"}
  |  -> {"type":"auth","access_token":"..."}
  |  <- {"type":"auth_ok","ha_version":"..."}      (or auth_invalid, and it hangs up)
  |
  |  -> {"id":1,"type":"assist_pipeline/run","start_stage":"stt",
  |      "end_stage":"intent","input":{"sample_rate":16000}}
  |  <- {"id":1,"type":"result","success":true}       the subscription is live
  |  <- event run-start   -> runner_data.stt_binary_handler_id = N
  |  <- event stt-start
  |
  |  => BINARY [N][...raw PCM...]           repeatedly, as it is captured
  |  <- event stt-vad-start {timestamp}     the server heard speech begin
  |  <- event stt-vad-end   {timestamp}     ...and end
  |  <- event stt-end       {stt_output:{text}}      <- the transcription
  |  => BINARY [N]                          one byte: end of stream
  |
  |  <- event intent-start
  |  <- event intent-end {intent_output:{response:{speech:{plain:{speech}}}}}
  |  <- event tts-start / tts-end {tts_output:{url}}   (end_stage "tts" only)
  |  <- event run-end
```

Four things are not obvious from the documentation and cost a day each.

**The audio format is fixed and undeclared.** `input.sample_rate` is the only
field. Everything else is assumed: signed 16-bit, little endian, mono, raw -
**no WAV header**, despite the metadata Home Assistant fills in internally
saying `AudioFormats.WAV`.

**Binary frames are multiplexed by a leading byte.** The connection carries
more than this pipeline, so every binary frame starts with the handler id from
`run-start` and the audio follows it. Home Assistant's dispatcher is literally
`handler = msg_data[0]; payload = msg_data[1:]`.

**A frame containing only that byte ends the stream.** The server's generator
is `while chunk := await audio_queue.get()`, so an empty payload is what stops
it. Forget it and the run sits until its timeout - 300 seconds by default.

**Voice activity detection is on and cannot be turned off for this stage.**
`no_vad` exists in the schema for the wake-word start stage only, and the
`input` sub-schema rejects unknown keys, so a push-to-talk client still has its
stream ended by the server after 0.7 s of silence. That is usually what you
want; it is not what you asked for, and a long pause mid-sentence ends the
utterance early. The same schema restriction is why `volume_multiplier`,
`auto_gain_dbfs` and `noise_suppression_level` are unavailable when starting at
`stt`: **a quiet microphone has to be amplified on the device**, or by raising
`Audio_PGA1_Setting`.

`end_stage` decides how much work the hub does. `stt` stops at the
transcription and asks no agent anything, which is all a dictation field needs
and the cheapest thing the pipeline can do. `intent` runs the conversation
agent as well. `tts` additionally synthesises a spoken reply and hands back a
URL to fetch it from.

## The crate

```
clients/couch-voice/
  src/abi.rs      the ALSA kernel ABI, verified by tools/alsa-abi-check.sh
  src/alsa.rs     Pcm, Capture: open, negotiate, poll, read, level, limit
  src/ctl.rs      the mixer: list, read and set controls by name
  src/level.rs    Meter for a UI; Analysis and Verdict for a probe
  src/wav.rs      Writer and Reader; the seam between the two halves
  src/ws.rs       RFC 6455 client, with the base64 and SHA-1 it needs
  src/ha.rs       auth, assist_pipeline/run, the events, the outcome
  src/main.rs     couch-voice, the CLI
  src/bin/couch-mic.rs   the probe
```

One trait joins the halves:

```rust
pub trait Source {
    fn rate(&self) -> u32;
    fn chunk_frames(&self) -> usize;
    fn read(&mut self, out: &mut [i16]) -> Result<usize>;
}
```

`alsa::Capture` implements it and so does `wav::Reader`. That is why the Home
Assistant client can be exercised end to end from a recording with no
microphone, and the capture path exercised on the device with nothing to talk
to. It is also how the tests work: `src/ha.rs` runs a whole pipeline against a
fake hub, feeding it samples from memory.

**Dependencies: two.** `serde_json` for the API's JSON, with no `serde` derive
anywhere - pipeline events stay `Value`, because a new Home Assistant release
must be able to add an event without this client failing to parse the
connection. And `libc`, for `ioctl` and `poll`; it contains no C, only extern
declarations against the musl that std already links.

Not depended on, and each for a reason in the source: `alsa`/`alsa-sys` (binds
libasound - see above), `tungstenite` (arrives with `rand`, `sha1`, `base64`,
`http`, `httparse`, `utf-8` and `byteorder` to buy server support and
extensions that a LAN hub does not use), `hound` (a WAV reader is 200 lines and
this one has to tolerate a header that was never patched).

## Security and privacy

A device with a microphone in somebody's living room. The rules, and where
each is enforced.

### Recording is always explicit, and always bounded

* **No wake word, no open mic, no timer.** The only way audio is captured is a
  call to `Pcm::configure`, which returns a `Capture` that is already running.
  There is no constructor that opens a device without starting it and no code
  path that opens one on an event. `MTK_VOW_SUPPORT` is `no` in this build, so
  the hardware's own always-listening block is not even compiled in.
* **Every stream carries a limit it cannot exceed.** `Wanted::limit` becomes a
  frame count inside `Capture`, checked before every read; when it is reached
  the stream stops itself and `read` returns 0, which every consumer already
  treats as the end. The default is 30 seconds. The limit lives at the lowest
  level on purpose - a caller cannot forget it, and a caller that hangs cannot
  extend it.
* **Dropping a `Capture` stops the stream and closes the device.** There is no
  way to hold one open with nothing reading it.
* **Nothing on the network can start a recording.** `couch-confd` has no such
  endpoint and must not gain one. The trust boundary for the config UI is
  "can you see the panel"; that is a fine boundary for editing a room list and
  a terrible one for turning on a microphone.
* **`couch-mic` cannot send audio anywhere.** It is a separate binary that
  imports no network module; the linker drops all of it, and the strings
  `Sec-WebSocket`, `api/websocket`, `assist_pipeline` and `access_token`
  appear zero times in the built probe against four, two, two and one in
  `couch-voice`.

### What the user interface owes them

Not built here - `ui/couch-gui/` belongs to someone else - but this is the
contract that makes the above worth anything, because a guarantee nobody can
see is not a guarantee.

* **An indicator that is present for exactly as long as the device is
  recording**, and impossible to miss: full width, high contrast, not a small
  icon in a corner. It must appear before the first sample is captured and
  disappear when the stream stops, including when it stops because the limit
  was reached or because an error killed it.
* **Drive it from the capture's own state, never from a flag somebody sets.**
  The indicator should be a function of "is there a live `Capture`", so that
  every path which stops recording also clears it and no future path can
  forget to. A boolean set at the start of `listen()` is the bug that ships.
* **A level meter beside it.** `Capture::level()` gives peak and RMS per
  period, and `Level::meter()` maps to 0..1 on a dB scale that looks alive.
  It tells the user two things at once: that it is hearing them, and that it
  really is on.
* **Say where the audio goes, once, in words**, at setup: which Home Assistant
  it is sending to. Not in a log; on screen.
* **A visible countdown or a shrinking bar for the cap.** Thirty seconds of
  recording that the user thinks stopped ten seconds ago is the exact failure
  the cap exists to bound, and showing it costs nothing.
* **Push to talk, released to stop.** `Capture::stop_handle()` gives a
  `StopHandle` that can be tripped from the key-event thread; the next read
  returns 0. Hold-to-talk is the honest gesture - the microphone is on while
  your finger is on it - and it is also what the server's VAD expects.

### The Home Assistant token

A long-lived access token from a Home Assistant user profile. It is a
credential with the full authority of that user.

* **Where it lives:** `/opt/couch/ha-token`, mode `0600`, owned by root, on the
  Alpine root - not in `/tmp`, which the setup portal and the pairing PIN
  already share, and not in the config the web UI can edit.
* **Which user issues it:** create a *dedicated, non-administrator* Home
  Assistant user for the remote. A token minted from an admin account lets
  anyone who lifts it off the flash reconfigure the house.
* **It is never on a command line.** There is deliberately no `--token` flag;
  passing one is a usage error that says why. A command line is visible to
  anything that can run `ps`.
* **It is never logged.** `Assistant` does not store the token after
  authenticating, its `Debug` prints only the endpoint and the Home Assistant
  version, and a refused token produces `Unauthorized { endpoint }` carrying no
  part of the credential. There is a test that asserts exactly that.
* **A file beats the environment**, which is why the file is checked first when
  `--token-file` is given: a process's environment is readable from `/proc` by
  anything running as the same user, and on this device everything runs as
  root. `COUCH_HA_TOKEN` works, and is second best.
* **World-readable token files are warned about, not refused.** Refusing would
  push someone towards putting the token somewhere worse.

### What leaves the device, and what is kept

* **What goes out:** raw 16-bit PCM, only while a run is in progress, only to
  the Home Assistant host given on the command line, over a WebSocket. Nothing
  else. Between runs the socket carries nothing.
* **It is not encrypted.** `ws://` on the LAN, as Home Assistant's own
  companion apps and Assist satellites do. Anything on the path can hear the
  audio and lift the token out of the `auth` message. On a home LAN with WPA2
  that is a smaller problem than it sounds, and it is real. `wss://` is not
  precluded: `ws::Transport` is a trait over `Read + Write` plus timeouts, and
  a TLS stream implementing it drops in with no other change. It is not
  implemented, because doing it properly means a certificate story for a hub
  that usually has a self-signed one.
* **What is written to disk on the device:** nothing, by `couch-voice listen`.
  `couch-voice record` and `couch-mic record` write a WAV to a path you name,
  and that is the only audio that persists - locally, and only because you
  asked. Delete `/tmp/mic.wav` after probing.
* **What Home Assistant keeps** is Home Assistant's business, and it is worth
  knowing before pointing a microphone at it. The pipeline keeps its **last 10
  runs in memory** per pipeline for the Assist debug page, including the
  transcription. If `assist_pipeline: debug_recording_dir:` is set in
  `configuration.yaml`, **it writes every utterance to disk as a WAV** - that
  is off by default and should stay off. And the audio goes wherever the
  configured speech-to-text engine sends it: a local Whisper add-on keeps it on
  the hub, Home Assistant Cloud sends it off the premises. That choice is made
  in Home Assistant, by whoever runs the house, and this client neither knows
  nor changes it.
* **Nothing is retained by this crate.** No buffer outlives a run; no cache, no
  history, no partial transcriptions on disk.

## Building

```sh
tools/build-voice.sh          # armv7 static musl, and the sizes
tools/build-voice.sh --host   # the same for this machine
tools/alsa-abi-check.sh       # the ALSA ABI, against the kernel's own header
cd clients && cargo test      # 27 tests here, none needing hardware or a hub
```

Prerequisites, once: `rustup target add armv7-unknown-linux-musleabihf`. The
ABI check additionally wants the Android NDK, for its `sound/asound.h` and an
armv7 clang; set `NDK=` if it is not at the path `gui/build.sh` uses.

### Sizes, measured

Release, `opt-level="s"`, LTO, stripped, static musl.

| Binary | armv7 | host (macOS arm64) |
|--------|------:|-------------------:|
| `couch-voice` | 505,140 (493 KB) | 436,240 |
| `couch-mic` | 448,816 (438 KB) | 402,400 |
| `couch-kodi`, for comparison | 570,500 | |
| `couch-confd`, for comparison | 1,225,080 | |

`couch-mic` is 56 KB smaller than `couch-voice` because the linker drops the
WebSocket and Home Assistant modules it does not reference. Source is 5,461
lines, of which 704 is the transcribed kernel ABI and about 900 is tests.

## The probes: exactly what to run on the device

None of this has touched the hardware. These are the programs that will.

Everything runs on the device and writes only to `/tmp` there. `couch-mic` has
no network code, so nothing it records can leave except by a file being copied
back deliberately.

**The one-liner**, which does all of it and brings back a WAV:

```sh
COUCH_IP=remote.local tools/mic-probe.sh          # -> build/mic.wav
```

It needs the device on WiFi with `sshd` running and a key enrolled (see
`docs/webui.md`). **Make a noise at the remote for the whole run** - talk at
it, tap its case. Silence proves nothing.

Or the same steps by hand, if you want to stop between them:

```sh
COUCH_IP=remote.local
COUCH_SSH_KEY=/path/to/couch-key
tools/build-voice.sh
scp -i "$COUCH_SSH_KEY" \
    clients/target/armv7-unknown-linux-musleabihf/release/couch-mic \
    root@"$COUCH_IP":/tmp/couch-mic
ssh -i "$COUCH_SSH_KEY" root@"$COUCH_IP" 'chmod 755 /tmp/couch-mic'
```

**1. What the card is, and what each capture device will accept.** Uses
`HW_REFINE`, which changes nothing and does not need a device to be startable,
so it is safe against a card in any state.

```sh
/tmp/couch-mic list
```

*What I need back:* the whole output. It answers whether `hw:0,1` really is
`MultiMedia1_Capture`, whether 16 kHz mono S16_LE is accepted, and what the
period-size range is.

**2. The mixer.** Prints the capture-path controls with their current values
and, for enumerated ones, every item they accept.

```sh
/tmp/couch-mic controls          # the capture path
/tmp/couch-mic controls -v       # all of them
```

*What I need back:* the whole output. This is the one that resolves the biggest
open question - what `Audio_MicSource1_Setting` and `Audio_ADC_1_Sel` will
actually take, which no amount of reading the kernel settles.

**3. The sweep.** Records 1.5 s from every capture device that accepts the
format and prints one line each ending in a blunt verdict - `DEAD`, `SILENT`,
`NOISE`, `CLIPPING` or `SIGNAL` - plus peak and RMS in dBFS and an ASCII
envelope, one character per 100 ms.

```sh
/tmp/couch-mic sweep
```

*What I need back:* the whole table. **Talk at the remote for the whole
sweep.** This is the command that answers "which one is the microphone".

**4. If nothing heard anything** - which is the likely first result, because
of the mixer trap above:

```sh
/tmp/couch-mic route --dry-run   # what it would set, and what those are now
/tmp/couch-mic route             # apply Android's own route
/tmp/couch-mic sweep             # then sweep again
```

*What I need back:* the `--dry-run` output and the result of each `route` line;
several will probably fail, and which ones fail is the information.

If a control refuses the value the route asks for, the error names what it
*will* take, and single controls can be tried by hand:

```sh
/tmp/couch-mic set Audio_MicSource1_Setting=ADC1 Audio_ADC_1_Switch=on
/tmp/couch-mic set Audio_PGA1_Setting=18Db       # if it is hearing, but quietly
```

**5. A proper recording**, once something hears.

```sh
/tmp/couch-mic record -D hw:0,1 -t 5 -o /tmp/mic.wav
```

Prints peak, RMS, DC offset, the distinct-value count, the zero count, the
envelope and a verdict, and leaves a WAV to listen to. `scp` it back and play
it; ears settle arguments that statistics do not.

The same analysis runs on the host over a file that already exists, which is
how a recording gets discussed after the fact:

```sh
clients/target/release/couch-mic analyse build/mic.wav
```

### Reading the verdicts

They are blunt on purpose, because the four ways a recording can be empty look
identical in a waveform:

| verdict | what it means |
|---------|---------------|
| `DEAD` | every sample is the same value. The DMA ran; nothing converted. Usually the ADC is off. |
| `SILENT` | moving, but below -80 dBFS peak. A live converter hearing nothing. |
| `NOISE` | a steady floor with no envelope over time. Powered, and either muted or not connected to a capsule. |
| `CLIPPING` | real audio, more than 1% of samples on the rail. Cut `Audio_PGA1_Setting`. |
| `SIGNAL` | at least 12 dB between the loudest and quietest 100 ms, and a peak above -45 dBFS. Something with a shape happened. |

The envelope is the discriminator that matters. A hiss and a voice have similar
RMS and completely different shapes over time.

## Then: talking to Home Assistant

Nothing here needs the microphone to work, so it can be done first.

Make a long-lived access token in Home Assistant (profile → Security → Long-
lived access tokens) for a **dedicated non-admin user**, and put it in a file:

```sh
umask 077; printf %s 'eyJ...' > ~/.couch-ha-token
```

From a laptop:

```sh
V="clients/target/release/couch-voice --host homeassistant.local \
   --token-file ~/.couch-ha-token"

$V version         # what is on the other end
$V pipelines       # which pipelines exist, and which have an stt engine
$V say "turn on the kitchen light"       # text only; no microphone involved
$V send build/mic.wav                    # a recording; still no microphone
```

`pipelines` marks the preferred one with `*` and prints each pipeline's
speech-to-text engine, because a pipeline with none will refuse an audio run
and that is the most common reason a first attempt fails.

On the device, with a working microphone:

```sh
printf %s 'eyJ...' > /opt/couch/ha-token && chmod 600 /opt/couch/ha-token
/opt/couch/couch-voice --host homeassistant.local -D hw:0,1 -t 8 listen
```

`--stage stt` stops at the transcription, which is what a text field wants and
the cheapest thing the hub can do. `--stage tts` additionally returns a URL for
a spoken reply.

## What has been verified, and how

* **The ALSA ABI**, against the kernel's own `sound/asound.h` compiled for
  32-bit ARM EABI. `tools/alsa-abi-check.sh`, and the same numbers as Rust
  assertions in `src/abi.rs`.
* **The WebSocket implementation**, against RFC 6455's own worked example for
  the handshake, the published SHA-1 and base64 vectors, and a socket-level
  round trip covering masking, a fragmented message, an interleaved ping that
  must be answered without surfacing, and a non-101 response.
* **The Home Assistant protocol**, against a fake hub that speaks real
  WebSocket and replays a scripted pipeline run. The tests assert that every
  binary frame carries the handler id, that the stream is terminated with a
  bare handler byte, that exactly the samples that went in came out, that a
  refused token produces `Unauthorized` with the token appearing nowhere in the
  error, and that an error event ends the run rather than hanging.
* **The whole CLI end to end**, against a Python stand-in for Home Assistant:
  `couch-voice send` on a two-second WAV delivered 64,000 bytes - 32,000 frames
  at 16-bit, exactly the file - and printed the transcription and reply the
  fake hub returned.
* **The signal analysis**, against synthesised digital silence, a stuck DC
  value, deterministic noise, a speech-shaped envelope and a clipped tone.
* **The microphone's existence**, from the vendor build configuration and the
  shipped kernel. Not from the hardware.

Nothing in the capture path has run against a real ALSA device. That is what
the probes are for.

## Open questions

1. **Is there a microphone, physically?** The vendor build says one analogue
   capsule. Only a recording proves it. → `couch-mic sweep`.
2. **Which capture device?** `hw:0,1` is the derivation; `couch-mic list`
   prints each device's own name. → step 1 of the probes.
3. **Does the analogue path need the mixer route, and which values?** Almost
   certainly yes, and unknown. → `couch-mic controls`, then `route`.
4. **What rates and formats does UL1 actually accept?** MediaTek's capture
   driver advertises 8k-48k S16_LE in the abstract; this board is unmeasured.
   → `couch-mic list`.
5. **Is the gain usable across a room?** A phone codec expects a capsule
   150 mm from a mouth. A remote on a coffee table is 3 m from a sofa, and
   `Audio_PGA1_Setting` may not have enough range. If not, the fix is digital
   gain on the device before the socket - Home Assistant's own
   `volume_multiplier` is unavailable when starting at the `stt` stage.
6. **Does the button gesture want push-to-talk or press-to-start?** Push to
   talk is the honest one and matches the server's VAD. It needs the keypad's
   release event, and this kernel's keypad has `linux,no-autorepeat` and a
   debounce that `tools/dtbpatch.py` already had to lower - so a held key
   producing a clean press/release pair is worth confirming before designing
   around it.
7. **Where does this live at runtime?** Today it is a CLI. The shape it wants
   is a small daemon that owns the capture and exposes a local socket to
   `couch-gui`, so that the GUI never holds the microphone and the indicator
   can be a function of the daemon's state. That is a design decision, not a
   detail.
8. **`wss://`.** The transport is abstracted for it and nothing implements it.
   Worth doing only alongside a certificate story for a self-signed hub.


## What the hardware actually did, measured

Everything below is from the HA100 itself, not from the kernel source. Three of
these were wrong in the first version of this crate, and each failed silently.

**The route table had `Audio_Preamp1_Switch=on`, which that control rejects.**
Its items are `OPEN | IN_ADC1 | IN_ADC2 | IN_ADC3`, and it is the switch that
connects the capsule to the ADC. Every other line in the table succeeded, so
routing reported success with the microphone disconnected, and the ADC then
sampled a floating pin. Correct value: `IN_ADC1`. `IN_ADC2` and `IN_ADC3` sit at
-0.3 dBFS, clipping - floating pins pulled to a rail, not capsules.

**The uplink is stereo and delivers two channels whatever it agrees to.** Asking
for one channel is accepted, and the driver then sends `L R L R` anyway: a five
second recording came back in 2.6 seconds of wall clock with twice the frames.
Read as consecutive mono samples that is speech at half speed - it sounds, in
the words of the person holding it, "like slow motion". The silent right channel
interleaving as an alternating zero also mirrors the whole spectrum above 4kHz,
which looks exactly like a broken capsule: 50% of the energy above 4kHz, a null
at 2-4kHz, and 14% in the telephone band. De-interleaved, the same recording is
73% telephone band and an ordinary speech shape. Ask for two channels.

**Only the left channel has a capsule on it.** Over 22 seconds of speech,
channel 0 measured -35 dBFS rms across 7066 distinct values and channel 1
-52 dBFS across 15. Averaging the two - which is what "one microphone, so it is
the same signal twice" implies - costs 6 dB on a device whose only gain control
is already at maximum. `demux` takes the first channel.

**There is no digital capture gain.** All 97 of the card's controls were listed.
`Audio_PGA1_Setting` is the only gain in the path and tops out at 24dB, which is
what the route table now sets. At that setting: noise floor about -36 dBFS mean
peak, speech close to the device peaking at -5 dBFS with 33 dB of SNR, and 25 dB
at arm's length. No clipping in 45 seconds. That is a usable microphone for
push-to-talk; it is not a far-field one, and there is no gain left to make it
one.

**EBUSY does not mean what it says.** Opening a capture device before the
analogue path is powered returns `EBUSY` with nothing holding the device. The
error text now says so, because otherwise it sends you looking for a process
that does not exist.

## sherpa-onnx on the remote: measured, not estimated

Whisper is the wrong shape for this device because its encoder pads every
utterance to thirty seconds. sherpa-onnx's streaming zipformer transducer does
work proportional to the audio instead, which is a different question, so it
was measured rather than reasoned about.

**It runs on the remote today, with no port.** The release's
`linux-arm-gnueabihf-static` build is static only in the sense that sherpa's own
libraries and onnxruntime are linked in; the binary itself still wants glibc.
But it needs exactly three things - `libc.so.6`, `libm.so.6` and
`ld-linux-armhf.so.3`, with no libstdc++ - so a Debian armhf libc6 unpacked into
a directory and invoked as an explicit loader is enough:

```sh
./glibc/ld-linux-armhf.so.3 --library-path ./glibc ./sherpa-onnx --tokens=... 
```

Alpine's `gcompat` was tried first and segfaults before printing its usage,
which is the usual outcome for a C++ binary that wants real TLS and unwinding.

**The numbers**, `sherpa-onnx-streaming-zipformer-en-20M-2023-02-17` int8
(43 MB), on the HA100:

| | |
|---|---|
| RTF, 23s of speech, 1 thread | 1.4 |
| RTF, 23s of speech, 4 threads | 1.6 - *slower* |
| RTF, 3s clip, 1 thread | 1.9 |
| cold start, 3s clip, end to end | 14.5s |
| peak RSS | 83 MB |

Three things follow. Threading does not help - four cores are slower than one,
the same memory-bandwidth ceiling the GUI's dirty-region work ran into, and for
the same reason. Fitting a line through the two RTF figures gives about 1.5s of
fixed warmup plus 1.35x the audio, so **the model must be resident**: 14.5s of
that cold start is loading 43 MB of weights, and paying it per utterance is
absurd where paying it once at boot is nothing.

With a warm, resident decoder fed while the user is still speaking, a
three-second phrase lands roughly two to three seconds after they stop. That is
usable for dictating a search - a D-pad keyboard takes twenty - and it is not
usable for anything conversational.

Accuracy was not established. The test recording came off a microphone whose
response is discussed above, and the transcript was recognisably English
without being right. Judging the model needs a clean input, which needs the
microphone question answered first.

## Accuracy, on this microphone, with known ground truth

Two sentences read into the remote through the mic key, recorded by `couch-gui`
at 16kHz mono and decoded by the 20M streaming zipformer on the device itself.

| said | heard |
|---|---|
| The birch canoe slid on the smooth planks | THE BIRCH CANOE SLID ON THE SMOOTH PLANKS |
| Play season **four** episode **two** of The **Sopranos** | PLAY SEASON **FOR** EPISODE **TO** OF THE **SOPRAMUS** |

The first sentence is a standard Harvard-list sentence and came back **without a
single error**, which answers the question the microphone section left open: the
capture path is good, and this model handles ordinary English on this hardware.

The second is 5 of 8, and every error is one of two kinds:

**Homophones.** "four" and "for", "two" and "to" are acoustically identical. No
microphone and no model size fixes them; only context does. In a media search
the context is absolute - "season for" is never right - so a table of the half
dozen number homophones, applied only after `season`, `episode`, `part` or
`volume`, resolves them completely.

**The proper noun.** "Sopranos" became "SOPRAMUS", which is exactly what a
20M model trained on read speech does with a title it has never seen.

sherpa-onnx has hotword biasing for this, but it wants the phrases in the
model's own token space and this model ships no `bpe.model`, so it cannot be
used without re-exporting. That is not worth doing, because the better fix is a
layer up: **the search target is a finite set we already have.** Kodi will list
every title in the library over JSON-RPC, and matching a noisy transcript into
that list is both easier and more robust than biasing a decoder.

Both together, against a ten-title stand-in library:

```
heard:    PLAY SEASON FOR EPISODE TO OF THE SOPRAMUS
resolved: PLAY SEASON FOUR EPISODE TWO OF THE SOPRANOS
```

"sopramus" matches "The Sopranos" at 0.75 similarity and nothing else close,
which is a comfortable margin - the errors a recogniser makes are phonetic, and
phonetic neighbours of a title in your library are usually that title.

So the accuracy answer is: good enough, provided the transcript is resolved
against the library rather than used raw. Which is the right design regardless,
because a search string that is not a title in the library is not useful.
