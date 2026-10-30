# docs

| File | Contents |
|------|----------|
| `protocol.md` | everything learned about the wire: DUML framing, the RC's USB mux and video channels, gimbal, thermal, flight-controller and OSDK frames, with capture dates and what was verified on the aircraft |
| `hardware.md` | the Orin's USB-C role handling, the serial adapters, the UART and USB API port wiring, video decoding on the Orin |
| `reverify.md` | the hardware re-verification checklist for the commits after e8a4c56 and the 2026-10-30 sweep fixes: each step, the log lines to expect, what passes |

Record findings with the date and how they were verified (capture, on the
aircraft, decoder output), and mark anything inferred but untested as such.
