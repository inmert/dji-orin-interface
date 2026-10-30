# scripts

Gadget bring-up and the run scripts. All but `run_local_llm.sh` need root
(configfs gadget, FunctionFS, raw USB). Put env vars after `sudo`.

| Script | Purpose |
|--------|---------|
| `gadget_lib.sh` | shared: teardown of the `dji` gadget, UDC handling |
| `run_server.sh` | create the accessory gadget, start `server` with `$CONFIG` (default `config.toml`; `config.dev.toml` for ground tests), bind the UDC once the descriptors are written |
| `restart_server.sh` | restart the server with the RC left plugged in; soft-reconnects the UDC if the RC does not re-enable the gadget |
| `restore_gadget.sh` | stop everything and rebind the stock `l4t` gadget |
| `run_capture.sh`, `run_bridge.sh` | the same bring-up for `ffs_capture` and `bridge` |
| `run_local_llm.sh` | start `llama-server` on the GPU as the local understanding model (`[understanding.local]`) |

Bring-up order matters: start the server, power the RC on, let it boot, then
plug the cable in (README "Bring-up order matters").
