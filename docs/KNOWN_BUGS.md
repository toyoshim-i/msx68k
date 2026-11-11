# Known Bugs

Bugs found but not fixed yet. Fixed entries are removed, with the fix described in its commit.

---

## 1. Desktop colors are black when the resolution is set before boot

**Symptom.** If the screen resolution is applied before SX-Window boots, the desktop comes up with
its gray palette entries black: text palette entries 1 to 3 and 8 to 11 stay `$0001` (black)
instead of the desktop grays (`$C5F1`, `$83E1`, `$FF7F`, ...), so only the SX logo shows. The
desktop is drawn, as TVRAM holds it; only the colors are missing.

**Reproduce.** Call `X68kBus::apply_screen_resolution` after `X68kVm::reset` and before booting,
at any extended width (1360, 1366, 1920 all show it), then boot SX-Window from the SCSI disk. The
same boot without that call, or switching resolution after the desktop is up
(`request_screen_resolution`), gives the normal palette.

**Where it matters.** The app applies the resolution before boot when it starts in fullscreen,
with `--fullscreen` or host native fullscreen saved in `MSX68K.LB`
(`crates/msx68k-app/src/main.rs`, after the window is created). Other initial resolutions and
switching from the settings app after boot are not affected.

**Not yet known.** The cause. SX-Window reads its palette at boot from the color settings in SRAM
(`$ED0072`..), and `apply_screen_resolution` also writes SRAM (`$ED007E`), so the early write may
disturb that, but this has not been checked.

**Tests.** `test_sxwin31_scsi_boot_with_1920x1080_initial_resolution` applies the resolution
before boot but checks only geometry, so it passes with the black palette.
