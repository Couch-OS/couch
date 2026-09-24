//! Provider-neutral camera decoding for the native Couch UI.
//!
//! A camera integration supplies Annex-B H264 bytes. This crate owns the only
//! decoder process, its fixed output shape and its resource limits. Network
//! addresses, credentials and provider-specific stream descriptors never
//! reach the decoder or its arguments.

use std::{
    io::Read,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    thread,
};

pub const WIDTH: u32 = 480;
pub const HEIGHT: u32 = 270;
pub const FRAME_BYTES: usize = WIDTH as usize * HEIGHT as usize * 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderFailure {
    Spawn(std::io::ErrorKind),
    MissingPipe,
    Input(std::io::ErrorKind),
}

/// A decoder process and its two bounded pipe endpoints.
///
/// The caller owns view lifetime and cancellation because those belong to the
/// source transport. Until `into_parts` succeeds, dropping this value kills
/// the child so a partial handoff cannot leak a decoder.
pub struct Decoder {
    child: Option<Child>,
    input: Option<ChildStdin>,
    output: Option<ChildStdout>,
}

impl Decoder {
    pub fn spawn() -> Result<Self, DecoderFailure> {
        let mut child = decoder_command().spawn().map_err(|error| {
            eprintln!("couch-camera: decoder spawn failed: {error}");
            DecoderFailure::Spawn(error.kind())
        })?;
        let Some(input) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(DecoderFailure::MissingPipe);
        };
        let Some(output) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(DecoderFailure::MissingPipe);
        };
        if let Some(errors) = child.stderr.take() {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = errors.take(8192).read_to_end(&mut bytes);
                if !bytes.is_empty() {
                    let text: String = String::from_utf8_lossy(&bytes)
                        .trim()
                        .chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .collect();
                    eprintln!("couch-camera: decoder stderr: {text}");
                }
            });
        }
        Ok(Self {
            child: Some(child),
            input: Some(input),
            output: Some(output),
        })
    }

    pub fn into_parts(mut self) -> (Child, ChildStdin, ChildStdout) {
        (
            self.child.take().expect("decoder child was present"),
            self.input.take().expect("decoder input was present"),
            self.output.take().expect("decoder output was present"),
        )
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn decoder_command() -> Command {
    // The GUI owns the framebuffer outside the Alpine chroot, where the
    // packaged decoder and its absolute ELF interpreter are below /mnt/alpine.
    // Enter that root through static BusyBox so the interpreter and libraries
    // resolve inside Alpine. In-chroot checks execute /usr/bin directly.
    let mut command = match std::env::var_os("COUCH_FFMPEG") {
        Some(decoder) => Command::new(decoder),
        None => packaged_decoder_command(
            std::path::Path::new("/usr/bin/ffmpeg").is_file(),
            std::path::Path::new("/mnt/alpine/usr/bin/ffmpeg").is_file(),
        ),
    };
    command
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-max_alloc",
            "16777216",
            "-protocol_whitelist",
            "pipe",
            "-threads",
            "1",
            "-probesize",
            "65536",
            "-analyzeduration",
            "500000",
            "-f",
            "h264",
            "-i",
            "pipe:0",
            "-an",
            "-sn",
            "-dn",
            "-filter_threads",
            "1",
            "-vf",
            "fps=8,scale=480:270:force_original_aspect_ratio=decrease,pad=480:270:(ow-iw)/2:(oh-ih)/2",
            "-threads",
            "1",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // Only async-signal-safe syscalls in the forked child. Cap malformed
        // compressed input before any decoder allocation; no core dumps.
        unsafe {
            command.pre_exec(|| {
                for (resource, limit) in [
                    (libc::RLIMIT_AS, 256 * 1024 * 1024),
                    (libc::RLIMIT_CPU, 90),
                    (libc::RLIMIT_CORE, 0),
                ] {
                    let value = libc::rlimit {
                        rlim_cur: limit,
                        rlim_max: limit,
                    };
                    if libc::setrlimit(resource, &value) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
    command
}

fn packaged_decoder_command(native: bool, mounted_alpine: bool) -> Command {
    if native || !mounted_alpine {
        Command::new("/usr/bin/ffmpeg")
    } else {
        let mut command = Command::new("/bin/busybox");
        command.args(["chroot", "/mnt/alpine", "/usr/bin/ffmpeg"]);
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_has_no_network_or_secret_arguments_and_fixed_output_bound() {
        let command = decoder_command();
        let args = command
            .get_args()
            .map(|s| s.to_str().unwrap())
            .collect::<Vec<_>>();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-protocol_whitelist", "pipe"]));
        assert!(args.windows(2).any(|pair| pair == ["-i", "pipe:0"]));
        assert!(args
            .iter()
            .all(|arg| !arg.contains("rtsps:") && !arg.contains("api_key")));
        assert_eq!(FRAME_BYTES, 388_800);
    }

    #[test]
    fn decoder_path_covers_in_chroot_and_outer_gui_roots() {
        let native = packaged_decoder_command(true, false);
        assert_eq!(native.get_program(), "/usr/bin/ffmpeg");
        assert!(native.get_args().next().is_none());

        let mounted = packaged_decoder_command(false, true);
        assert_eq!(mounted.get_program(), "/bin/busybox");
        assert_eq!(
            mounted
                .get_args()
                .map(|arg| arg.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["chroot", "/mnt/alpine", "/usr/bin/ffmpeg"]
        );

        assert_eq!(
            packaged_decoder_command(true, true).get_program(),
            "/usr/bin/ffmpeg"
        );
        assert_eq!(
            packaged_decoder_command(false, false).get_program(),
            "/usr/bin/ffmpeg"
        );
    }
}
