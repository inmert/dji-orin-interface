# dji-tools

Capture and bridge binaries for learning the protocol. All need root and the
real USB hardware; `scripts/run_*.sh` set the gadget up and tear it down.

| Binary | Role | Use |
|--------|------|-----|
| `ffs_capture` | Orin as the DJI app toward the Cendence RC (gadget) | log everything the RC sends an app |
| `aoa_capture` | Orin as the RC toward a CrystalSky running DJI Pilot (host) | log Pilot's command stream |
| `bridge` | both at once: a transparent DUML MITM between Pilot and the RC | watch Pilot fly the real aircraft; `BRIDGE_DUMP_DIR` saves both raw streams |

Pure passthrough: nothing is injected by any of these. The findings they
produced are written up in `docs/protocol.md`.
