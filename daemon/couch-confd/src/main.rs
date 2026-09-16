//! The Couch config server.
//!
//! One static binary that owns `/opt/couch/config.json` and serves both the
//! REST API over it and the web UI that drives that API. It also owns media
//! control connections through a private Unix socket shared with the GUI.
//! Network work runs outside the GUI rendering loop.
//!
//! It does not replace `stage2/portal.sh`. The setup portal answers the
//! question "which network should this join", runs on port 80 out of an AP the
//! device hosts itself, and has to work before there is any network at all;
//! this answers "what is in your house", and needs one.

mod api;
mod assets;
mod auth;
mod store;

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use api::Api;
use assets::Assets;
mod local_name;
use auth::Auth;
use store::Store;

const DEFAULT_ADDR: &str = "0.0.0.0:8090";

/// tiny_http hands one request at a time to whoever calls `recv`, so
/// concurrency is however many threads are in this loop. Four, because a
/// browser opens up to six connections to one origin and a single worker would
/// let one kept-alive idle connection stall the page - but this is a
/// four-core device serving one person, so a pool is a bound, not a scaling
/// story.
const WORKERS: usize = 4;

struct Options {
    addr: String,
    config: String,
    www: Option<String>,
    pin_file: String,
    no_auth: bool,
}

fn main() {
    let options = match parse_args() {
        Ok(Some(options)) => options,
        Ok(None) => return,
        Err(message) => {
            eprintln!("couch-confd: {message}");
            eprintln!("try --help");
            std::process::exit(2);
        }
    };

    let store = match Store::open(&options.config) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("couch-confd: cannot use {}: {e}", options.config);
            std::process::exit(1);
        }
    };
    println!(
        "couch-confd: config {} at revision {}",
        store.path().display(),
        store.revision()
    );

    let assets = match &options.www {
        Some(dir) => {
            println!("couch-confd: serving the web UI from {dir}");
            Assets::from_dir(dir)
        }
        None => {
            let embedded = Assets::embedded();
            if embedded.is_empty() {
                // Not fatal: the API is the useful half and this is the normal
                // state of a build done before the frontend exists.
                println!("couch-confd: no web UI embedded - the API is still up");
            } else {
                println!(
                    "couch-confd: {} embedded asset(s), {} KB",
                    embedded.count(),
                    embedded.bytes() / 1024
                );
            }
            embedded
        }
    };

    let bound = resolve(&options.addr);
    let server = match tiny_http::Server::http(bound) {
        Ok(server) => Arc::new(server),
        Err(e) => {
            eprintln!("couch-confd: cannot bind {}: {e}", options.addr);
            std::process::exit(1);
        }
    };
    println!("couch-confd: listening on http://{}", options.addr);
    // On the device, also answer on the plain port and for couch.local, so
    // the address in a browser is a name and nothing else. Neither is fatal:
    // the port may be taken and multicast may be down; :8090 still works.
    let plain =
        local_name::plain_listener(&bound).and_then(|addr| match tiny_http::Server::http(addr) {
            Ok(server) => {
                println!("couch-confd: also listening on http://{addr}");
                Some(Arc::new(server))
            }
            Err(e) => {
                println!(
                    "couch-confd: port {} unavailable ({e}); use :{}",
                    addr.port(),
                    bound.port()
                );
                None
            }
        });
    if local_name::is_device_listener(&bound) {
        let port = plain
            .as_ref()
            .map_or(bound.port(), |_| local_name::PLAIN_PORT);
        match local_name::advertise(port) {
            Ok(()) => println!(
                "couch-confd: answering for http://{}{}",
                local_name::display_host(),
                if port == local_name::PLAIN_PORT {
                    String::new()
                } else {
                    format!(":{port}")
                }
            ),
            Err(e) => println!(
                "couch-confd: not advertising {}: {e}",
                local_name::display_host()
            ),
        }
    }

    let auth = Arc::new(Auth::new(&options.pin_file, options.no_auth));
    if auth.disabled() {
        // Loud, because the whole point of the PIN is that nobody has to trust
        // the LAN, and this hands that back.
        println!("couch-confd: *** --no-auth: anything on this network can rewrite the config ***");
    } else {
        println!(
            "couch-confd: pairing by PIN, shown via {}",
            options.pin_file
        );
    }

    // A PIN nobody follows up on has to leave the panel by itself. Checking
    // expiry only when a request arrives got that exactly backwards: the case
    // where the prompt is unwanted is the case where no request ever comes.
    {
        let auth = auth.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            auth.reap();
        });
    }

    couch_control::serve(&store.path().with_file_name("control.sock"))
        .expect("start private control socket");
    let api = Arc::new(Api::new(store, assets, auth));
    // A Bluetooth pairing window opened for a device ends on the HID daemon's
    // side; the bond it made is stored on the device from here, once a
    // second, because nothing else that sees the daemon's state can write
    // the configuration.
    {
        let api = api.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            api.tick();
        });
    }
    let mut workers = Vec::new();
    for _ in 1..WORKERS {
        let (server, api) = (server.clone(), api.clone());
        workers.push(std::thread::spawn(move || serve(&server, &api)));
    }
    if let Some(plain) = plain {
        for _ in 0..WORKERS {
            let (server, api) = (plain.clone(), api.clone());
            workers.push(std::thread::spawn(move || serve(&server, &api)));
        }
    }
    serve(&server, &api);
    for worker in workers {
        let _ = worker.join();
    }
}

fn serve(server: &tiny_http::Server, api: &Api) {
    for request in server.incoming_requests() {
        api.handle(request);
    }
}

/// Bind addresses are resolved here rather than handed to tiny_http as a
/// string, so that a typo fails with the address in the message.
fn resolve(addr: &str) -> SocketAddr {
    match addr.to_socket_addrs().ok().and_then(|mut it| it.next()) {
        Some(resolved) => resolved,
        None => {
            eprintln!("couch-confd: cannot resolve {addr}");
            std::process::exit(2);
        }
    }
}

/// Hand-rolled rather than clap: three options, and clap is 300KB of binary
/// and a dozen crates to parse them. The same trade `couch-kodi` makes with
/// its HTTP client.
fn parse_args() -> Result<Option<Options>, String> {
    let mut options = Options {
        addr: env_or("COUCH_CONFD_ADDR", DEFAULT_ADDR),
        config: env_or("COUCH_CONFIG", store::DEFAULT_PATH),
        www: std::env::var("COUCH_WWW").ok(),
        pin_file: env_or("COUCH_PIN_FILE", auth::DEFAULT_PIN_FILE),
        no_auth: std::env::var("COUCH_NO_AUTH").is_ok(),
    };

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("couch-confd {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--addr" => options.addr = value()?,
            "--config" => options.config = value()?,
            "--www" => options.www = Some(value()?),
            "--pin-file" => options.pin_file = value()?,
            "--no-auth" => options.no_auth = true,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(Some(options))
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_string())
}

fn print_help() {
    println!(
        "couch-confd - the Couch config server

Usage: couch-confd [options]

  --addr ADDR     listen here (default {DEFAULT_ADDR}, env COUCH_CONFD_ADDR)
  --config PATH   the config file (default {default_config}, env COUCH_CONFIG)
  --www DIR       serve the web UI from DIR instead of the embedded copy
                  (env COUCH_WWW) - for the dev loop on a laptop
  --pin-file PATH where the pairing PIN is written for couch-gui to show
                  (default {default_pin}, env COUCH_PIN_FILE)
  --no-auth       no pairing at all (env COUCH_NO_AUTH) - for a laptop with no
                  remote to read a PIN off. Never on a device.
  -h, --help      this
  -V, --version   the version

An empty config file is created if it does not exist.

A browser pairs by entering a four-digit PIN that appears on the remote's own
screen: the credential is being able to see it.",
        default_config = store::DEFAULT_PATH,
        default_pin = auth::DEFAULT_PIN_FILE
    );
}
