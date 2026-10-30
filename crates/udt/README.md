# dji-udt

A receive-only UDT client for the FPV camera's video, which the aircraft
serves over its USB network (`192.168.42.2:40003`).

| File | Purpose |
|------|---------|
| `vendor/udt4/` | UDT 4.11 from DJI's Onboard SDK, unchanged (BSD licence) |
| `shim.cpp` | a C API over the C++ library: connect, recv, close |
| `build.rs` | compiles the vendored C++ with the system `c++` and `ar` |
| `src/lib.rs` | the safe `Stream`: connect with a timeout, receive, drop closes |

Only UDT's own handshake and ACKs ever go to the aircraft; there is no send
path for payload data.
