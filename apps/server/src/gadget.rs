//! The USB gadget side: the FunctionFS accessory gadget's ep0 event loop,
//! and the RC link workers it starts on the endpoints the RC enables.
//!
//! `scripts/run_server.sh` binds the UDC after the server logs `gadget
//! ready`. The workers survive a DISABLE/ENABLE cycle (an RC power-cycle) on
//! their own; one that exited (endpoint gone) is respawned on the next
//! ENABLE. Without a gadget (run outside the script) the web UI is served
//! on its own and the main thread parks.

use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::{JoinHandle, sleep};
use std::time::Duration;

use dji_common::logln;
use dji_transport::aoa::device::answer_setup;
use dji_transport::usb::ffs::{self, Event};
use dji_vehicle::Vehicle;
use dji_vehicle::links::rc;

use crate::video::Feed;

/// Park the main thread forever, keeping the spawned threads alive.
fn park() -> ! {
    loop {
        sleep(Duration::from_secs(3600));
    }
}

/// Whether a worker slot needs (re)spawning: never started, or exited.
fn needs_spawn(h: &Option<JoinHandle<()>>) -> bool {
    h.as_ref().is_none_or(JoinHandle::is_finished)
}

/// Write the descriptors to `ffs_dir` and serve its events forever. `rc_feeds`
/// take the RC mux's video channels; `dump` is a file for the raw RC bytes.
pub fn run(
    ffs_dir: &str,
    vehicle: Arc<Vehicle>,
    rc_feeds: Vec<Arc<Feed>>,
    dump: Option<PathBuf>,
) -> ! {
    let ep0 = match ffs::init_ep0(ffs_dir) {
        Ok(fd) => fd,
        Err(e) => {
            logln!("gadget: init failed ({e}); serving web UI only, no USB link");
            park();
        }
    };
    logln!("gadget ready; waiting for the RC to enable it");

    let mut reader_h: Option<JoinHandle<()>> = None;
    let mut writer_h: Option<JoinHandle<()>> = None;
    ffs::run_events(ep0.as_fd(), |event| match event {
        Event::Enable => {
            logln!("gadget: ENABLE");
            let open = |name| ffs::open_endpoint(ffs_dir, name, 200);
            if needs_spawn(&reader_h) {
                match open("ep1") {
                    Ok(ep1) => {
                        let (vehicle, rc_feeds, dump) =
                            (Arc::clone(&vehicle), rc_feeds.clone(), dump.clone());
                        reader_h = Some(std::thread::spawn(move || {
                            rc::read(ep1, &vehicle, dump.as_deref(), |ch, payload| {
                                if let Some(f) =
                                    rc_feeds.iter().find(|f| f.rc_channel() == Some(ch))
                                {
                                    f.offer(payload);
                                }
                            })
                        }));
                        logln!("link up: telemetry reader running");
                    }
                    Err(e) => logln!("gadget: {e}"),
                }
            }
            if needs_spawn(&writer_h) {
                match open("ep2") {
                    Ok(ep2) => {
                        let vehicle = Arc::clone(&vehicle);
                        writer_h = Some(std::thread::spawn(move || rc::write(ep2, &vehicle)));
                        logln!("link up: RC writer running");
                    }
                    Err(e) => logln!("gadget: {e}"),
                }
            }
        }
        Event::Disable => {
            logln!("gadget: DISABLE");
            vehicle.telemetry.lock().unwrap().link_up = false;
        }
        Event::Setup(s) => {
            let _ = answer_setup(ep0.as_fd(), &s);
        }
        Event::Bind => logln!("gadget: BIND"),
        Event::Unbind => logln!("gadget: UNBIND"),
        Event::Other(_) => {}
    });
}
