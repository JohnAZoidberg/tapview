# PixArt touch orientation

PixArt touchpad controllers can swap and flip the X/Y coordinates they report. The setting is a single user register, so tapview's `--write-user-reg` can change it on a running pad, without reflashing. Like every user register it is RAM: the firmware loads it from its parameter section at boot, so a pad reset or power loss puts the firmware's value back.

## The register

ORIENTATION is user bank 2 (Navigation), register `0x04`, in PixArt PTHIDUtility's register tables for the PJP215, PJP255, PJP274, PJP342, PJP343, PJP352 and PJP360 (PCT1036). On the PLP239 it is bank 3, `0x04`, with the same bits.

| bit | meaning |
|-----|---------|
| 0   | swap X and Y |
| 1   | flip X |
| 2   | flip Y |

The swap is applied before the flips. As a whole, relative to the pad's native scan order (directions with Y pointing down, as evdev reports it):

| value | reported coordinates |
|-------|----------------------|
| `0`   | native |
| `1`   | X and Y swapped (mirrored across the diagonal) |
| `2`   | mirrored left-right |
| `3`   | rotated 90° clockwise |
| `4`   | mirrored top-bottom |
| `5`   | rotated 90° counter-clockwise |
| `6`   | rotated 180° |
| `7`   | swapped, then rotated 180° |

The firmware follows a write immediately, with no apply step. On a Framework Laptop 13 (PJP343, `093A:0343`) `2:0x04=6` turned the pad by 180° on the spot, and `2:0x04=0` turned it back:

```
tapview --device 1 --write-user-reg 2:0x04=6
tapview --device 1 --write-user-reg 2:0x04=0
```

Always pass `--device`. Without it `--write-user-reg` goes to the default touchpad, which on a laptop is the built-in one, even when the values were meant for an external pad.

## Coordinate range

Two more bank 2 registers set how far the coordinates go: WIDTH (`0x00` low, `0x01` high) and HEIGHT (`0x02` low, `0x03` high). Each is the maximum of a native axis before the swap, and it equals the HID descriptor's logical maximum plus one. Writing the high byte applies the new value, so write the low byte first; `--write-user-reg` writes in command-line order.

| pad | WIDTH | HEIGHT | ORIENTATION | evdev X / Y max |
|-----|-------|--------|-------------|-----------------|
| Framework Laptop 13, PJP343 | 2834 (`0x0b12`) | 1724 (`0x06bc`) | `0x00` | 2833 / 1723 |
| Touchpad KB (daisy), PCT1036, firmware 0x1204 | 999 (`0x03e7`) | 797 (`0x031d`) | `0x01` | 796 / 998 |

The HID descriptor is fixed, so the reported range has to keep fitting it:

- Changing only the flip bits keeps both ranges and is always safe.
- Toggling the swap bit moves WIDTH onto the other axis. Unless WIDTH and HEIGHT are swapped along with it, one axis reports past its logical maximum and the other covers only part of its range. With them swapped, the ranges fit again but the picture is stretched, because the descriptor's units per millimetre do not change.

## Touchpad KB (daisy)

Daisy firmware 0x1204's parameter section sets ORIENTATION to `0x01`: it swaps X and Y rather than rotating. Its pad reports 0-796 by 0-998 at 12 units/mm, a 66 x 83 mm portrait surface. Older parameter sets that leave `0x04` unset report the native scan order.

Relative to what daisy ships with:

| value | output | WIDTH / HEIGHT |
|-------|--------|----------------|
| `1`   | as shipped | 999 / 797, unchanged |
| `3`   | mirrored left-right | unchanged |
| `5`   | mirrored top-bottom | unchanged |
| `7`   | rotated 180° | unchanged |
| `2`   | rotated 90° clockwise, stretched | 797 / 999 |
| `4`   | rotated 90° counter-clockwise, stretched | 797 / 999 |
| `0`   | native scan order, stretched | 797 / 999 |
| `6`   | native scan order rotated 180°, stretched | 797 / 999 |

The stretched rows put the 83 mm axis on X at 797 units and the 66 mm axis on Y at 999 units, so X motion comes out at 0.8x and Y at 1.25x. For example, 90° clockwise, and back to as shipped:

```
tapview --device <N> \
  --write-user-reg 2:0x00=0x1d --write-user-reg 2:0x01=0x03 \
  --write-user-reg 2:0x02=0xe7 --write-user-reg 2:0x03=0x03 \
  --write-user-reg 2:0x04=2

tapview --device <N> \
  --write-user-reg 2:0x00=0xe7 --write-user-reg 2:0x01=0x03 \
  --write-user-reg 2:0x02=0x1d --write-user-reg 2:0x03=0x03 \
  --write-user-reg 2:0x04=1
```

Daisy has to be attached by USB cable (`32ac:0034`). Through the USB-A dongle register reads come from the dongle's own cache, so tapview cannot identify the chip and refuses to write. These writes have not yet been tried on daisy itself.
