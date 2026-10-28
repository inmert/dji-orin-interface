# UDT4 (vendored)

UDT 4.11, the UDP-based reliable transport by Yunhong Gu (University of
Illinois), copied unmodified from DJI's Onboard SDK 4.1
(`osdk-core/advanced-sensing/camera_stream/udt/src`, commit `cd8d881d`
"restore original UDT source 4.11"). DJI's SDK uses it to read the aircraft's
camera streams from `192.168.42.2:4000x` over the USB network link.

BSD 3-clause licence: see `LICENSE` and the header of each file. Built by
`../../build.rs`; the C API the Rust side uses is in `../../shim.cpp`.
