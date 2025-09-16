# Hootux

Hootux was created with the intention of learning how a kernel works and started from
[Phill Opp's blog: how to write an os in rust](https://os.phil-opp.com/). This project was not made just to write
"Hello, world" to the display from bare metal, but to create a proper usable kernel that **should** be able to be used
for
general purpose work. The idea is to become a POSIX compatible unix like monolithic kernel.

As much as I'd like for this to be used for proper work I know it won't. Before beginning this project I knew nothing
about kernel design, and I will continue to keep myself somewhere in the dark until I'm ready to compare this
project with ones that have been thought through. This is an ambitious project for someone of my capability so this
project will probably evolve quite slowly. I've created this project to learn, so it'd be stupid of me to not share this
knowledge. If you've stumbled on this project and want more information feel free to create an issue and ask even if it
seems stupid.

## Building

### Dependencies

- Rust is required to build this project. Installation instructions can be found [here](https://rustup.rs/)
- A nightly version of the rust compiler is required which can be installed with `rustup toolchain install nightly`
- The `x86_64-unknown-none` nightly target must be installed which can be done with `rustup +nightly target add
  x86_64-unknown-none
- NASM is required to compile some assembly.

### Actually building

Hootux is built and run via the root crate which acts as a runner the built binaries can be found thi the `target`
directory

- `cargo build`          : will build the binaries normally without running

Note: There is currently a bug in the artifact dependencies feature that causes it to resolve features incorrectly.

### Running

Hootux comes with a runner for running with qemu-system-x86_64 which can run bia the following commands.

- `cargo run -- --uefi`  : will boot hootux in qemu using ovmf firmware
- `cargo run -- --bios`  : will boot hootux in qemu using legacy boot
- `cargo run -- --help`  : will provide a help message with extra arguments and their usages

Booting with uefi firmware requires the `runcfg.toml` to be set correctly. It requires at the very least to be pointed
to a working uefi firmware image at the key `uefi.bios` This project will not provide a working firmware. I also
recommend setting an `uefi.vars` a efivars file.

## TODO

This part is mostly for my own sanity to try to manage what I want to do and to lay out the things I need to to to
achieve them.

- Remove combinedAllocator in favour of a solution that doesn't suck.
- Replace InterruptIndex with abstract IRQ type
    - Replace all its uses
    - Ok maybe it should be a struct wrapper around it.
        - This allows a proper kernel interrupt API using a private InterruptIndex
- Unify initialization of ACPI consumers
- Clean up memory module
    - make buddy allocators use the same code
        - use a marker for optimizing it.
- AHCI driver
    - Refactor it
        - It's currently a can of spaghetti
- Fix and write tests
    - because I really need to
- Add gdbstub
    - do mp first. to allow tasks to be run asynchronously while the debugger is doing things.
- ~~Multiprocessing~~
    - NUMA?
    - thread local allocators?
    - Add TLB synchronization to allocator to prevent shootdowns
        - Add dirty linear-memory free-list where linear addresses are freed into.
- Add caching mechanism
    - Because it seems useful and interesting to implement
    - I need to figure out the best way to do this too.
    - I'd like to use an ARC algorithm, but it seems a bit over my head.
- ANSI support
    - ~~This will require creating a font module using bitmaps not rasters~~
        - Just take a font from Linux and use `bindgen`
- ~~VFS~~
    - Support FAT filesystems
    - EXT too maybe?
    - Do psudoFSs first
    - ~~RAM-disk support~~
        - With quotas
        - This doesn't seem too hard.
- Investigate making drivers the sole owners of PCI devices
    - Kernel requires some ownership, pass a dyn Into<pci::DeviceControl> to the kernel
- Fix PCI ownership models
    - Current idea, save al PCI functions in static arr of &dyn PciDevice drivers must implement this and place
      themselves into the arr
        - This allows the kernel to have control through the static arr, requests cna be processed by the driver and the
          driver must reconfigure the pci dev.
    - Also for BAR memory.
        - Make kernel alloc the memory and keep a Arc<Option<*\[u8\]>> into it share it between all things that want to
          access it.
- Upgrade logger
    - Add log buffer in memory to be recalled later
    - Print time in messages
    - Print module in messages
        - Debug and trace should have the full fn name
        - Others should just mention the module name
            - Except in debug mode, do the full path
- Move PCI into its own driver crate
- ~~Add USB support~~
    - Add support for periodic transactions
        - And Isochronus transactions
    - Unify public types to allow other controller types with the same API
    - Fix bug where device addresses for EHCI is freed too early
    -
- Create tooling to parse and interpret QEMU `-d` and trace outputs
- Swanky async optimization using union to contain future on the stack when the future is smaller than or the same sizes
  as the return type.
    - This allows for maybe-async functions by just returning the completed result and polling it to return Ready(_)
    - Will this even work?
- Create arch-agnostic library to allow for architecture agnostic types instead of relying on either x86_64 or terrible
  design decisions
    - I have a half-baked almost-prototype already
- Optimise memory initialisation, it wasn't ever very fast but legacy boot proves just how abysmally slow it really is.
- ~~Determine whether DmaTarget should be `where Self: 'static`~~
    - Change DmaTarget into a struct
        - Prevents the issue where Box<DmaBuffer<_,Vec<u8>> must be used, and complicated borrowing
        - DmaTarget should be able to be constructed from any buffer, and cast back to generic (Box,Vec) buffers.
        - This should allow accessing data as `&mut [T]`.