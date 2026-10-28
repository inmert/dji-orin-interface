//! Default serial ports for the direct aircraft readers ([`crate::flight`]),
//! chosen again on every open attempt so a device plugged in (or renumbered)
//! after startup is still found.
//!
//! * SDK UART: the CP210x USB-TTL adapter when one is plugged in (the path that
//!   works both ways), else the Orin's own UART, `/dev/ttyTHS1`.
//! * USB API port: the first `ttyACM` that is not the CrystalSky. After an
//!   aircraft restart its port may not come back, and the CrystalSky then
//!   takes `ttyACM0`; reading the tablet as the aircraft is worse than nothing.

use std::path::Path;

/// udev's stable names for USB serial devices.
const BY_ID: &str = "/dev/serial/by-id";
/// The Orin's own UART on the 40-pin header (pins 8/10).
const ORIN_UART: &str = "/dev/ttyTHS1";
/// Name prefix of a Silicon Labs CP210x adapter under [`BY_ID`].
const CP210X: &str = "usb-Silicon_Labs_CP210";
/// The CrystalSky (DJI ZS600B) under [`BY_ID`].
const CRYSTALSKY: &str = "ZS600B";

fn list(dir: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// The SDK UART to read: a CP210x adapter if present (as its device, e.g.
/// `/dev/ttyUSB0`, for a readable UI), else the Orin's UART.
pub fn osdk_uart() -> String {
    let path = pick_uart(&list(BY_ID));
    std::fs::canonicalize(&path)
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or(path)
}

fn pick_uart(by_id: &[String]) -> String {
    by_id
        .iter()
        .find(|n| n.starts_with(CP210X))
        .map_or_else(|| ORIN_UART.into(), |n| format!("{BY_ID}/{n}"))
}

/// The aircraft's USB API port: the first `ttyACM` that isn't the
/// CrystalSky, or `None` while there is none.
pub fn usb_acm() -> Option<String> {
    let tablet: Vec<String> = list(BY_ID)
        .iter()
        .filter(|n| n.contains(CRYSTALSKY))
        .filter_map(|n| std::fs::canonicalize(Path::new(BY_ID).join(n)).ok())
        .filter_map(|p| p.file_name()?.to_str().map(String::from))
        .collect();
    pick_acm(&list("/dev"), &tablet).map(|n| format!("/dev/{n}"))
}

/// The lowest-numbered `ttyACM<n>` in `dev` not in `tablet`.
fn pick_acm(dev: &[String], tablet: &[String]) -> Option<String> {
    dev.iter()
        .filter_map(|n| Some((n.strip_prefix("ttyACM")?.parse::<u32>().ok()?, n)))
        .filter(|(_, n)| !tablet.contains(n))
        .min()
        .map(|(_, n)| n.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prefers_the_cp210x_adapter_over_the_orin_uart() {
        let by_id = names(&[
            "usb-DJI_ZS600B_1TSB3JW27Y-if02",
            "usb-Silicon_Labs_CP2102_USB_to_UART_Bridge_Controller_0001-if00-port0",
        ]);
        assert_eq!(
            pick_uart(&by_id),
            "/dev/serial/by-id/usb-Silicon_Labs_CP2102_USB_to_UART_Bridge_Controller_0001-if00-port0"
        );
        assert_eq!(
            pick_uart(&names(&["usb-DJI_ZS600B_1TSB3JW27Y-if02"])),
            ORIN_UART
        );
        assert_eq!(pick_uart(&[]), ORIN_UART);
    }

    #[test]
    fn skips_the_crystalsky_when_choosing_the_aircraft_port() {
        let dev = names(&[
            "tty", "ttyACM0", "ttyACM1", "ttyACM10", "ttyTHS1", "ttyACMx",
        ]);
        // Aircraft first, tablet next (both plugged in).
        assert_eq!(
            pick_acm(&dev, &names(&["ttyACM1"])).as_deref(),
            Some("ttyACM0")
        );
        // Tablet took ttyACM0: the aircraft is the next one.
        assert_eq!(
            pick_acm(&dev, &names(&["ttyACM0"])).as_deref(),
            Some("ttyACM1")
        );
        // Only the tablet: no aircraft port.
        assert_eq!(pick_acm(&names(&["ttyACM0"]), &names(&["ttyACM0"])), None);
        // Numeric, not lexical, order.
        assert_eq!(
            pick_acm(&names(&["ttyACM10", "ttyACM2"]), &[]).as_deref(),
            Some("ttyACM2")
        );
    }
}
