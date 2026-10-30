# Hardware re-verification

What to check on the aircraft before trusting the commits after e8a4c56
(18e147c config and module split, 697ffcf schema key order, 0e59380 confirmed
plans and motor tickets, 6b6577f capability registry) and the sweep fixes on
branch `review/sweep-2026-10-30` (48dd16f tests, 49befc9 fixes). Everything
here passed in CI against stand-ins for the RC, the FC and the UART; this
session checks the real ones.

**Props off for the whole session.** Sections D to I spin the motors.

Each step lists what to do, the log lines to expect in `logs/server.log`
(watch with `tail -f logs/server.log`), and what counts as a pass. Write the
date and the outcome next to each step, and keep the log.

## Before you start

- On the Orin, branch `review/sweep-2026-10-30` checked out, then
  `cargo build --release` and `(cd web && npm ci && npm run build)`.
- Close VS Code's rust-analyzer and Firefox before building (memory).
- Have the OSDK app id at hand; the CP210x adapter on the SDK UART; the
  aircraft's USB API port connected only for section F.
- The RC powered and booted *before* its cable is plugged in.
- Optional: run on `config.dev.toml` (tighter plan limits, still enough for
  H17/H18) by putting `CONFIG=$PWD/config.dev.toml` after `sudo`; A1's
  `config:` line then names that file.
- If the log stops at `gadget: BIND`, check the three cases under
  "Bring-up order matters" in the README (Type-C mode, USB role switch, RC
  stuck in `default`) before replugging again.

## A. Start-up and settings (18e147c)

1. `sudo OSDK_APP_ID=<id> scripts/run_server.sh`, then plug the RC in (or
   `scripts/restart_server.sh` with it already connected).

   Expect, in order:
   - `config: /home/dog/repos/dji-orin-interface/config.toml`
   - `keys: anthropic key from …/secrets/openai.key` and `keys: deepgram key from …`
   - `understand: model providers [anthropic claude-opus-5-5]; using anthropic`
   - `voice: speech providers [deepgram]; using deepgram`
   - `ws: accepting Host ["localhost", "127.0.0.1", "::1", "dog-station", "10.77.77.239", …]`
   - `http: serving …/web/dist on 0.0.0.0:8080` and `https: serving … on 0.0.0.0:8443`
   - `flight: reading /dev/ttyUSB0 (receive-only)`
   - `gadget ready; waiting for the RC to enable it`, then `gadget: ENABLE`,
     `link up: telemetry reader running`, `link up: RC writer running`

   Pass: all of these; no `motors: no osdk_app_id` line (the id was given);
   no `config:` error.

2. Open the portal at `http://<orin-ip>:8080/` **and** at
   `https://dog-station:8443/` (or however you normally reach it).

   Pass: both show "Aircraft connected" and live frame counts. If the page
   loads but stays "Connecting to server", the log has
   `ws: refused origin …`: the name you used is not one of the Orin's own
   (e.g. `dog-station.local`). Add it to `[web] hosts` in `config.toml`,
   restart, and note it here.

## B. Read-only telemetry and video

3. Flight view: UART panel "Receiving", attitude follows when you tilt the
   aircraft by hand. USB link view (section F's cable) and Streams view live.
4. Start video on camera 1, camera 2 and FPV.
   Expect `video cam1: opening RC channel 0x11`, `video cam1: receiving; decoding`
   (same for cam2 and fpv). Pass: three moving pictures.

## C. Gimbal and thermal (writer moved in 18e147c)

5. Drag each joystick; release. Recenter camera 1, camera 2, Recenter all.
   Link cameras, move camera 1, unlink.
   Expect `rc: yaw recenter sent to camera 1`, `rc: camera lock engaged` /
   `released`. Pass: as on 2026-10-28.
6. Thermal: IR, click a spot, People preset, Auto.
   Expect `thermal: camera accepted seq …` for each setting.

## D. Motors over the RC link (props off)

7. Press **Arm**, confirm.
   Expect: `ws: motors start queued`, `motors: start requested`,
   `rc: function 0x07 sent (seq N)`,
   `flyc: 0x03/0x2a FlightCtrl[0] seq N type 0x80 payload [00]`,
   `motors: motors started over the RC link`.
   Pass: the panel says Armed · RC link; FC status On ground.
8. Press **Disarm**. Expect the same with `0x08` and
   `motors: motors stopped over the RC link`; panel Disarmed.

## E. Motors over the UART (RC link down, props off)

9. Unplug the RC cable (`gadget: DISABLE`). Press **Arm**.
   Expect `motors: start requested`, then
   `motors: motors started over the UART; control released`.
   Pass: motors on; the RC sticks still have control afterwards (control
   released). Then **Disarm**: `motors stopped over the UART; control released`.
10. **Disarm while a start is in progress (fix H4).** Press **Arm**, and
    within a second, while the panel still shows "starting motors…", press
    **Disarm** (it must be clickable).
    Expect one of:
    - `motors: start failed: start abandoned for a stop; control released`
      followed by `motors: stop requested` and `motors stopped over the UART`;
    - or, if the start had already reached the FC, `motors started …` then
      straight away `motors: stop requested` and `motors stopped …`.

    Pass: the motors end stopped, in either case within a couple of seconds.
11. Plug the RC back in (`gadget: ENABLE`, `link up: …` only if a worker had
    exited). Check D7 once more.

## F. USB flight unlock (USB API port connected)

12. With the API port connected, **Arm** is refused by the FC. Press
    **USB flight**. Expect `motors: motors allowed with USB connected (until restart); control released`.
    Then D7 and D8 again: pass if they now succeed.

## G. Command bar: Check and Run (697ffcf, 0e59380, 6b6577f)

13. Type `arm for 3 seconds then disarm`, press **Check**.
    Expect `understand: anthropic claude-opus-5-5 in … ms: arm, then wait 3 s, then disarm`
    and `plan: checked "arm for 3 seconds then disarm" (claude-opus-5-5): …`.
    Pass: the bar shows the three steps, source claude-opus-5-5.
14. Press **Run**, confirm.
    Expect `plan: started confirmed "…" (claude-opus-5-5): …`,
    `plan: step 1: arm`, the D7 lines, `plan: step 2: wait 3 s`,
    `plan: step 3: disarm`, the D8 lines, `plan: done`.
    Pass: FC status On ground for about 3 s, then Stopped.
15. Check `Could you spin the motors up for four seconds, then swing the second camera right for a second and a half and recenter everything`.
    Pass: all five steps shown (arm, wait 4, disarm, pan camera 2 right 1.5, recenter); do not run it.
16. **Refusals (fixes H2, H3).** Check each, run none:
    - `arm` → refused, "the plan would leave the motors running: end it with a disarm"
      (or the model declines it first).
    - `take off` → `understand: anthropic declined: …`; no plan; no grammar line after it.
    - `move up for 2 seconds` → declined as aircraft movement; it must **not** become
      a gimbal tilt (before 49befc9 it did).

## H. Cancel and failure handling (props off)

17. Check and Run `arm for 20 seconds then disarm`. During the wait press **Cancel**.
    Expect `plan: cancel requested`, the stop lines, `plan: cancelled; motors stopped`.
18. Check and Run `arm for 20 seconds then disarm`; during the wait press
    **Disarm** on the motors bar. Expect the stop; the plan's own disarm step then
    answers either `motors stopped …` or an FC refusal (already off), and the plan
    ends `done` or `step 3 failed: …; motors already stopped`. Pass: motors stopped,
    nothing left running.

## I. Voice (HTTPS only)

19. At `https://<orin>:8443/`, hold **Talk**, say "arm for three seconds then
    disarm", release.
    Expect `voice: deepgram … ms, … bytes: "Arm for three seconds, then disarm."`
    then the G13 lines. Pass: the sentence appears already checked; run it (G14).
20. Tap **Talk** briefly (shorter than the microphone takes to open, fix M7).
    Pass: the button returns to "Talk"; no `voice:` line, no upload.

## J. Restart without replugging

21. `sudo OSDK_APP_ID=<id> scripts/restart_server.sh` with the RC connected.
    Expect `RC link up (gadget enabled)` from the script and the A1 lines.
    Pass: portal reconnects by itself; D7/D8 work.

## K. Finish

22. `sudo scripts/restore_gadget.sh`. Keep `logs/server.log` with this file's
    notes. If everything passed, fast-forward `main` to the branch.

## If something fails

Stop, Disarm, and note the step and the log lines around it. A motor step that
reports a failure but left the motors running is the most important thing to
report (fix H1 should make the plan stop them: its message then ends
`; motors stopped` or `; MOTORS MAY BE RUNNING: stop them`).
