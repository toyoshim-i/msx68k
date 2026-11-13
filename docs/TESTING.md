# Testing

## Fast development loop

Use unit tests for a focused logic change, and the relevant VM scenarios for
behavior that depends on Human68k or SX-Window. Run emulator integration tests
in release mode; debug builds spend much longer interpreting guest code.

```sh
cargo test --workspace --lib --bins --locked -- --test-threads=1
cargo test --release -p msx68k-vicon --test vmx68k_lifecycle --locked -- --nocapture
```

The unit command is serial because some tests measure host timing. Avoid running
another build or test suite concurrently when checking those timing tests.

## Graphics without a guest OS

Use the ROM-free graphics and raster fixtures for scanout changes. They check
expected pixel colors before comparing CPU, primary GPU and VM texture output;
the raster fixture also executes a real 68000/MFP interrupt handler. This keeps
the usual development loop independent of a Human68k or SX-Window boot.

```sh
cargo test -p msx68k-bus --test gvram --test vram_pages --test bg --test raster --locked -- --test-threads=1
cargo test --release -p msx68k-vicon --test graphics_modes --test raster --locked -- --test-threads=1
```

A GPU adapter is optional: the color assertions still run on the CPU reference
when no adapter is available. See [GRAPHICS](GRAPHICS.md) for timing scope.

## Selecting VM scenarios

The runner's registry is the source of scenario names and order. List it without
booting a VM:

```sh
MSX68K_CASES=list cargo test --release -p msx68k-vicon --test vmx68k_lifecycle --locked -- --nocapture
```

Set `MSX68K_CASES` to an exact name or a one-based index. Commas select several
items; `..` selects an inclusive range, with names or indices as endpoints. For
example, this runs the first three scenarios and the fifth:

```sh
MSX68K_CASES=1..3,5 cargo test --release -p msx68k-vicon --test vmx68k_lifecycle --locked -- --nocapture
```

Selection preserves registry order. An unset variable runs the whole suite;
unknown names and invalid ranges fail before boot. `--nocapture` shows the
primary's boot time and each scenario's start, result, and elapsed time. Compile
time is separate from these measurements. Do not copy scenario names or a case
inventory into this document; use the listing command above.

## Shared-primary lifecycle

One invocation boots the primary SX-Window VM once. Selected scenarios then run
sequentially inside it: start a VM, perform operations and assertions, and close
it normally through its window. Earlier changes to the primary remain visible;
neither RAM nor disk snapshots are restored between scenarios. Fixture files
are supplied through normal guest file operations, and backing images are
shared rather than rewritten for every scenario.

Normal exit must release guest VM/window registrations, disk bridges, pending
requests, VM thread owners, DOS file handles, and FD passthrough reservations.
The physical write-protect tab must remain intact. The test controller has its
own persistent window and bridge; only those deliberate resources may remain
between scenarios. Normal teardown must not manually detach a guest's bridge to
make these checks pass. Panic/timeout cleanup may cancel bridges before joining
threads so that reporting a failure cannot hang.

A leak that breaks a later scenario is a test failure, not a reason to restore
an independent state before each case. The suite stops on its first failure.
Use subsets to compare an isolated scenario with the sequence leading up to it.
Independent execution of every remaining scenario after a failure is not
required.

### Interpreting a failure and narrowing it down

A failure in a later scenario does not necessarily mean that scenario's feature
is broken. An earlier VM may have left a file handle, thread, request, window or
FD reservation behind. The next scenario exposing it is intentional coverage
of repeated use of the same SX-Window session. Earlier valid file changes can
also expose a mistaken assumption in a test; distinguish those expected side
effects from resources that should have been released.

1. Read the first `FAIL` entry and assertion. Resource-check failures identify
   the kind of resource that did not return to the expected state. Otherwise,
   include the preceding scenarios in the investigation.
2. Run the failed scenario alone using its name or index from the listing.
   Set `MSX68K_CASES` on the command above; do not change the registry to filter.
   If it still fails, investigate that scenario's behavior and setup first.
3. If it passes alone, run the inclusive range from the start through the failed
   scenario. A failure only in the sequence points to retained state, ordering,
   or an earlier shutdown problem. Passing alone does not invalidate it.
4. Move the range's starting point forward, or select suspected predecessors
   together with the failing scenario using commas, to find a smaller
   reproducing sequence. More than one predecessor may be needed. Selected
   scenarios still execute in registry order within one shared primary.

For example, with an illustrative failure at index 5, compare `MSX68K_CASES=5`,
`MSX68K_CASES=1..5`, and then smaller selections such as `3..5` or `2,5`.
The numbers illustrate selection syntax, not a maintained list of test cases.
Each command invocation starts a new primary once; there is no reset between
its selected scenarios. Keep that property while investigating, or the cleanup
bug being sought can disappear.

Guest commands still pass through the emulated keyboard and operating system.
Queue scan codes and wait for observable completion, such as command output,
changed file contents, or a completed redraw. Avoid per-key wall-clock delays
and fixed sleeps. Keep bounded deadlines so a regression fails with context.

## Cold boot and other checks

Cold-boot behavior is tested separately from reuse of a running primary:

```sh
cargo test --release -p msx68k-bus --test scsi_boot -p msx68k-vicon --test vmx68k_disk --locked -- --test-threads=1
cargo build --release --workspace --locked
```

Other integration targets live in each crate's `tests/` directory. Select those
relevant to the change rather than rerunning every hardware/rendering test for
each edit.

Guest tests need the local ROMs and disk images: in particular `roms/IPLROM.DAT`,
`roms/CGROM.DAT`, `disks/HUMAN302.XDF`, and the SX-Window installation disks.
Some tests skip when assets are unavailable; check the output before treating
a run as validation. GPU tests also need an available `wgpu` adapter.
VMX68K scenarios build their own in-memory primary disk; they do not modify the
user's installed HDD image. Set `MSX68K_SCREENSHOT_DIR` to an existing directory
when UI screenshots are needed.

## Adding coverage

Put logic tests beside the implementation. Add guest integration scenarios to
`crates/msx68k-vicon/tests/vmx68k_lifecycle.rs` and its `support/` modules, with a
stable descriptive registry name. Reuse the session, exercise normal shutdown,
and leave lifecycle checks enabled. Use scenario-specific output paths so an
old file cannot satisfy a new assertion. Add a separate cold-boot test only when
initialization itself is the behavior under test.
