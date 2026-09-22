//! Serving the web UI: the table build.rs baked in, or a directory on disk.
//!
//! The directory mode exists for the dev loop. `trunk serve` proxies its own
//! rebuilds, but the thing being tested here is usually the *daemon*, and
//! rebuilding it to pick up a CSS change is the wrong shape of wait.

use std::fs;
use std::path::{Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

/// Says, in the bytes of the binary itself, that this daemon was built with
/// protocol 3 switched on (docs/development/protocol.md, "The switch").
///
/// A Cargo feature leaves nothing behind that release tooling can see: the
/// source tree, the version and the file names are the same either way. So the
/// build scripts, the payload inventory and the update publisher all search
/// `couch-confd` for this line, and the publisher refuses to sign a binary that
/// carries it under anything but a `.p3.dev` version. The value is computed
/// from the switch itself and not from a second `cfg`, so the two cannot drift
/// apart: a daemon that admits protocol 3 carries the line, and no other does.
/// `#[used]` keeps it through LTO and the linker's garbage collection although
/// nothing reads it; tools/build-webui.sh checks that on every build.
#[used]
static PREVIEW_BUILD: &[u8] =
    if couch_plugin::accepted_protocol_version() > couch_plugin::PROTOCOL_VERSION {
        b"COUCH-PREVIEW-BUILD protocol-3\n"
    } else {
        b""
    };

/// Shown when the binary was built with no `dist` directory to embed.
///
/// A blank page here would be indistinguishable from a routing bug, and this
/// is the state of every fresh clone until trunk has run once.
const PLACEHOLDER: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>couch-confd</title>
<style>body{font:16px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;
background:#0f1115;color:#e6e9ef;margin:0;padding:40px 20px}
main{max-width:34rem;margin:0 auto}code{background:#10131a;padding:2px 6px;border-radius:4px}
a{color:#4c8dff}</style>
<main>
<h1>No web UI in this build</h1>
<p>The API is up - try <a href="/api/config"><code>/api/config</code></a> - but this
binary was compiled with no <code>web/couch-web/dist</code> to embed.</p>
<p>Build the frontend and rebuild the daemon:</p>
<pre><code>tools/build-webui.sh</code></pre>
<p>Or point a running daemon at a dist directory with
<code>--www web/couch-web/dist</code>.</p>
</main>
"#;

pub struct Assets {
    /// When set, files are read from here instead of the embedded table.
    dir: Option<PathBuf>,
}

pub struct Asset {
    pub body: Vec<u8>,
    pub content_type: &'static str,
    /// Trunk fingerprints its output, so those may be cached forever; the
    /// entry point that names them must never be.
    pub immutable: bool,
}

impl Assets {
    pub fn embedded() -> Assets {
        Assets { dir: None }
    }

    pub fn from_dir(dir: impl Into<PathBuf>) -> Assets {
        Assets {
            dir: Some(dir.into()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dir.is_none() && ASSETS.is_empty()
    }

    pub fn count(&self) -> usize {
        match &self.dir {
            Some(_) => 0,
            None => ASSETS.len(),
        }
    }

    /// Total embedded bytes, for the startup line - the number that shows up in
    /// the binary's size.
    pub fn bytes(&self) -> usize {
        ASSETS.iter().map(|(_, b)| b.len()).sum()
    }

    /// Resolve a URL path.
    ///
    /// Anything not found falls back to `index.html`, because the frontend
    /// routes in the browser: a reload on `/areas/kitchen` must reach the app,
    /// not a 404. Requests under `/api` never get here.
    pub fn get(&self, url_path: &str) -> Option<Asset> {
        let name = url_path.trim_start_matches('/');
        let name = if name.is_empty() { "index.html" } else { name };

        if let Some(found) = self.read(name) {
            return Some(found);
        }
        // A path with an extension that missed is a genuine 404 - serving HTML
        // where a script was asked for produces a syntax error in the console
        // and nothing that says what really happened.
        if Path::new(name).extension().is_some() {
            return None;
        }
        self.read("index.html").or_else(|| {
            Some(Asset {
                body: PLACEHOLDER.as_bytes().to_vec(),
                content_type: "text/html; charset=utf-8",
                immutable: false,
            })
        })
    }

    fn read(&self, name: &str) -> Option<Asset> {
        if !is_safe(name) {
            return None;
        }
        let body = match &self.dir {
            Some(dir) => fs::read(dir.join(name)).ok()?,
            None => ASSETS
                .iter()
                .find(|(route, _)| *route == name)
                .map(|(_, bytes)| bytes.to_vec())?,
        };
        Some(Asset {
            content_type: content_type(name),
            // Trunk's fingerprinted names contain a hash; index.html does not.
            immutable: name != "index.html" && has_fingerprint(name),
            body,
        })
    }
}

/// Reject anything that could climb out of the web root.
///
/// Only meaningful in `--www` mode - the embedded table has no filesystem
/// behind it - but the check lives here so both paths get it and nobody has to
/// remember which is which.
fn is_safe(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name
            .split('/')
            .any(|part| part == ".." || part == "." || part.is_empty())
}

/// Trunk names its output `couch-web-<16 hex>.js`, `style-<16 hex>.css` - and,
/// for the wasm, `couch-web-<16 hex>_bg.wasm`, where wasm-bindgen's own suffix
/// lands after the hash. So the test is "some `-` or `_` separated part of the
/// stem is a long run of hex", not "the stem ends in one": the tidier version
/// silently gave the largest file in the bundle a no-cache header.
///
/// Getting this wrong in the other direction only costs a revalidation.
fn has_fingerprint(name: &str) -> bool {
    Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .split(['-', '_'])
        .any(|part| part.len() >= 8 && part.chars().all(|c| c.is_ascii_hexdigit()))
}

fn content_type(name: &str) -> &'static str {
    match Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
    {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        // Without this exact type the browser refuses instantiateStreaming and
        // the app fails with a message about the MIME type, not about the app.
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The release tooling trusts one thing about this line: it is in the
    /// binary exactly when the daemon admits protocol 3. This test binary is
    /// built from the same module with the same features, so it is its own
    /// evidence, whichever way it was built. The line is spelt backwards here,
    /// and turned round behind `black_box`, so that neither the test nor an
    /// optimiser puts it in the binary. The search is `str::contains` over a
    /// megabyte at a time because that is compiled into std and takes
    /// milliseconds; a byte loop of this crate's own takes seconds over a
    /// debug binary.
    #[test]
    fn the_preview_line_is_in_the_binary_exactly_when_protocol_3_is_on() {
        use std::io::Read;
        let on = couch_plugin::accepted_protocol_version() > couch_plugin::PROTOCOL_VERSION;
        let line: String = std::hint::black_box(b"3-locotorp DLIUB-WEIVERP-HCUOC")
            .iter()
            .rev()
            .map(|byte| char::from(*byte))
            .collect();
        if on {
            assert_eq!(PREVIEW_BUILD, format!("{line}\n").as_bytes());
        } else {
            assert!(PREVIEW_BUILD.is_empty());
        }
        let mut binary = fs::File::open(std::env::current_exe().unwrap()).unwrap();
        let mut window = Vec::new();
        let mut found = false;
        loop {
            // Keep the tail of the last read, so a line across two reads is seen.
            let keep = window.len().saturating_sub(line.len() - 1);
            window.drain(..keep);
            let read = binary
                .by_ref()
                .take(1 << 20)
                .read_to_end(&mut window)
                .unwrap();
            if read == 0 {
                break;
            }
            // Lossy conversion replaces what is not UTF-8 and leaves ASCII alone.
            found |= String::from_utf8_lossy(&window).contains(&line);
        }
        assert_eq!(found, on);
    }

    #[test]
    fn traversal_is_refused() {
        assert!(!is_safe("../etc/passwd"));
        assert!(!is_safe("a/../../b"));
        assert!(!is_safe("/etc/passwd"));
        assert!(is_safe("index.html"));
        assert!(is_safe("assets/app.css"));
    }

    #[test]
    fn fingerprints_are_recognised() {
        assert!(has_fingerprint("couch-web-3a5f9c11d2e4b607.js"));
        assert!(has_fingerprint("couch-web-3a5f9c11d2e4b607_bg.wasm"));
        assert!(has_fingerprint("style-da4c81658f66dd3a.css"));
        assert!(!has_fingerprint("index.html"));
        assert!(!has_fingerprint("couch-web.js"));
        assert!(!has_fingerprint("favicon.ico"));
    }

    #[test]
    fn wasm_gets_its_own_type() {
        assert_eq!(content_type("x_bg.wasm"), "application/wasm");
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
    }

    #[test]
    fn a_missing_script_is_a_404_not_the_app() {
        let assets = Assets::embedded();
        assert!(assets.get("/nope.js").is_none());
        // ... but a route the frontend owns reaches the app.
        assert!(assets.get("/areas/kitchen").is_some());
    }
}
