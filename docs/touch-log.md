# Logging touch activity

`--log-touches FILE` runs tapview without a window and appends one CSV row
per touch to FILE until it is killed. A touch runs from the first contact
down to the last one up. The log records when each pad is touched, for how
long and with how many fingers, and how long it then sits untouched. It
was written to feed a touchpad power model, where the time between touches
decides how long the pad stays in its run state. Positions are not logged.

```
tapview --log-touches ~/touches.csv                   # every touchpad
tapview --device event22 --log-touches ~/touches.csv  # just this one
```

Without `--device` it follows every touchpad, including ones attached
while it runs: it looks for new pads every 2 s (every 10 s on Windows, plus
whenever Raw Input delivers a report from a pad it has not seen). With
`--device` it follows that one pad and finds it again by name and USB/HID
IDs after a reconnect, when its node may have changed (`event22` to
`event25`).

It uses the same touch backend as the window: evdev on Linux, Raw Input
on Windows, and the HID or MultitouchSupport reader on macOS. It needs the
same access as the window too: the `input` group on Linux, nothing special
on Windows.

## Format

```
event,time,device,duration_s,max_contacts,contacts,clicks,detail
log,1793854296.518,,,,,,"tapview 0.1.0, all touchpads"
start,1793854296.523,"PIXA3854:00 093A:0343 (I2C 093a:0343)",,,,,"/dev/input/event8"
touch,1793854296.831,"PIXA3854:00 093A:0343 (I2C 093a:0343)",0.448,2,2,0,
start,1793854298.527,"Framework Touchpad KB (Bluetooth 32ac:0018)",,,,,"/dev/input/event22"
touch,1793854301.034,"Framework Touchpad KB (Bluetooth 32ac:0018)",0.500,1,1,1,
lost,1793854304.744,"Framework Touchpad KB (Bluetooth 32ac:0018)",,,,,"read error: No such device (os error 19)"
sleep,1793855000.120,,3612.415,,,,
alive,1793860000.004,,,,,,
```

| event | time | other columns |
|---|---|---|
| `log` | the logger started | `detail`: version, which pads |
| `start` | a pad was found and opened | `device`; `detail`: its node or interface path |
| `touch` | first contact down | `device`; `duration_s` until the last contact lifts; `max_contacts` at once; `contacts` that landed during the touch; `clicks`, button presses |
| `lost` | a pad went away (unplugged, Bluetooth disconnect) | `device`; `detail`: the error, or `removed` when a rescan no longer finds it |
| `sleep` | the system went to sleep (suspend, or Windows froze the process in modern standby) | `duration_s` |
| `alive` | still logging; written after 5 min without another row, so the end of a run is known | |

`time` is Unix seconds. A pad is covered from its `start` row to its
`lost` row, or to the last row before the next `log` row: when the logger
was stopped is only known to within the 5 min heartbeat. A restart appends
to the same file after a new `log` row.

Details:

- A lift followed by a new contact within 50 ms counts as the same touch.
  This absorbs the empty frames that Windows hybrid-mode reports parse to.
  It is also far shorter than any pad's run timeout.
- Sleep is detected when the wall clock jumps more than 4 s past the
  logger's 1 s wait. The resolution is about a second, and a large manual
  clock change would show up as a sleep.
- Touch lengths come from the backend's frame timestamps: kernel stamps on
  Linux, receipt time on Windows. Start times use the wall clock when the
  first frame is received.
- On Linux and macOS a pad attached while the logger runs is picked up on
  the next rescan, so a touch in its first 2 s can be missed. On Windows
  its first report triggers the rescan.

## Running it in the background

Linux, as a transient user service that stops at logout:

```
systemd-run --user --unit tapview-touchlog \
    "$PWD/target/release/tapview" --log-touches $HOME/touches.csv
journalctl --user -u tapview-touchlog    # messages
systemctl --user stop tapview-touchlog
```

Windows, hidden, from PowerShell (Raw Input needs the interactive session,
so run it as the logged-in user, not as a service):

```
Start-Process tapview.exe -WindowStyle Hidden -ArgumentList '--log-touches', "$env:USERPROFILE\touches.csv"
Stop-Process -Name tapview
```

To start it at every logon, wrap the same `Start-Process` line in a
scheduled task: Task Scheduler, trigger "At log on", "Run only when user is
logged on".
