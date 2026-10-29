//! Auto range for the thermal camera: keeps the XT2's isotherm band fitted to
//! the temperatures in view, so its colours always span the scene.
//!
//! While [`Thermal::auto`](super::Thermal) is on, once a second: if the camera is not measuring the whole image, queue full-frame
//! area measurement; otherwise read the area's minimum and maximum and, when
//! they have left the band or the band has grown much wider than them, queue
//! the writes that move the band to `[floor(min) - 1, ceil(max) + 1]`. The
//! hysteresis (4 °C of slack) keeps it from chasing every tenth of a degree.

use std::thread::sleep;
use std::time::{Duration, Instant};

use dji_common::logln;
use dji_protocol::thermal::{AreaStats, Measure};

use super::{Band, ISOTHERM_RANGE, ThermalCmd, band_of, isotherm_sequence};
use crate::Vehicle;

const PERIOD: Duration = Duration::from_secs(1);
/// Repeat an area-measurement setup step at most this often while the camera hasn't
/// switched (each request is one command to the aircraft).
const SETUP_RETRY: Duration = Duration::from_secs(5);
/// After queuing a band change, wait this long for the report to echo it.
const SETTLE: Duration = Duration::from_secs(2);
/// Margin added below the minimum and above the maximum, °C.
const MARGIN: f32 = 1.0;
/// Refit once the band is this much wider than the scene on either side, °C.
const SLACK: f32 = 4.0;

/// The whole image, as an area rectangle.
const FULL_FRAME: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// The band the scene calls for, if the current one no longer fits it.
pub fn auto_band(area: &AreaStats, cur: Band) -> Option<Band> {
    let (min, max) = (area.min, area.max);
    if !(min.is_finite() && max.is_finite()) || min > max {
        return None;
    }
    let (lower, upper) = (f32::from(cur.0), f32::from(cur.2));
    let fits = min >= lower && max <= upper && min - lower <= SLACK && upper - max <= SLACK;
    if fits {
        return None;
    }
    let clamp = |v: f32| {
        let (lo, hi) = (
            f32::from(*ISOTHERM_RANGE.start()),
            f32::from(*ISOTHERM_RANGE.end()),
        );
        // In range after the clamp, so the cast cannot truncate.
        v.clamp(lo, hi) as i16
    };
    // Leave room for the minimum band width without exceeding the accepted range.
    let lower = clamp((min - MARGIN).floor()).min(*ISOTHERM_RANGE.end() - 2);
    let upper = clamp((max + MARGIN).ceil()).max(lower.saturating_add(2));
    let middle = lower + (upper - lower) / 2;
    Some((lower, middle, upper))
}

fn is_full_frame(area: &AreaStats) -> bool {
    area.rect
        .iter()
        .zip(FULL_FRAME)
        .all(|(got, want)| (got - want).abs() < 0.01)
}

/// Keep the band fitted while auto range is on, forever.
pub fn run(vehicle: &Vehicle) {
    let thermal = &vehicle.thermal;
    let mut last_setup: Option<(ThermalCmd, Instant)> = None;
    let mut last_band: Option<Instant> = None;
    loop {
        sleep(PERIOD);
        if !thermal.auto() {
            last_setup = None;
            continue;
        }
        let Some(report) = vehicle.telemetry.lock().unwrap().thermal else {
            continue;
        };
        let area = report.area.filter(is_full_frame);
        let Some(area) = area else {
            // One step per tick: the camera ignores a rectangle sent while it
            // is still switching to area measurement (Pilot also waits).
            let step = if report.measure == Measure::Area as u8 {
                ThermalCmd::Area(FULL_FRAME)
            } else {
                ThermalCmd::Measure(Measure::Area)
            };
            if last_setup.is_none_or(|(prev, t)| prev != step || t.elapsed() >= SETUP_RETRY) {
                let mut q = thermal.lock();
                if !q.auto || !q.capacity(1) || q.pending.contains(&step) {
                    continue;
                }
                q.pending.push(step);
                last_setup = Some((step, Instant::now()));
                logln!("thermal auto: setting up full-frame area measurement: {step:?}");
            }
            continue;
        };
        if last_band.is_some_and(|t| t.elapsed() < SETTLE) {
            continue;
        }
        let cur = band_of(&report);
        let Some(target) = auto_band(&area, cur) else {
            continue;
        };
        let mut q = thermal.lock();
        if !q.auto
            || !q.capacity(5)
            || q.pending
                .iter()
                .any(|t| matches!(t, ThermalCmd::Isotherm(..)))
        {
            continue;
        }
        q.pending.extend(isotherm_sequence(cur, target));
        last_band = Some(Instant::now());
        logln!(
            "thermal auto: scene {:.1}..{:.1} °C, band {cur:?} -> {target:?}",
            area.min,
            area.max
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(min: f32, max: f32) -> AreaStats {
        AreaStats {
            rect: FULL_FRAME,
            min,
            max,
            ..AreaStats::default()
        }
    }

    #[test]
    fn fits_the_band_around_the_scene_with_hysteresis() {
        // Pilot's leftover fire band is far too wide for a room: refit.
        assert_eq!(
            auto_band(&area(17.2, 20.7), (27, 177, 695)),
            Some((16, 19, 22))
        );
        // Already fitted (within the slack): leave it alone.
        assert_eq!(auto_band(&area(17.6, 20.4), (16, 19, 22)), None);
        // Something hotter enters the view: widen.
        assert_eq!(
            auto_band(&area(17.2, 36.5), (16, 19, 22)),
            Some((16, 27, 38))
        );
        assert_eq!(
            auto_band(&area(1000.0, 1000.0), (0, 1, 2)),
            Some((798, 799, 800))
        );
        // Nonsense from the camera is ignored.
        assert_eq!(auto_band(&area(f32::NAN, 20.0), (16, 19, 22)), None);
        assert_eq!(auto_band(&area(30.0, 20.0), (16, 19, 22)), None);
        // A flat scene still gets a band of at least 2 °C, within the range.
        assert_eq!(
            auto_band(&area(20.0, 20.0), (-40, 0, 800)),
            Some((19, 20, 21))
        );
        assert_eq!(
            auto_band(&area(-80.0, 2000.0), (0, 1, 2)),
            Some((-40, 380, 800))
        );
    }
}
