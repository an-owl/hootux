# Hootux

Hootux was created with the intention of learning how a kernel works and started from 
[Phill Opp's blog: how to write an os in rust](https://os.phil-opp.com/). This project was not made just to write 
"Hello, world" to the display from bare metal, but to create a proper usable kernel that **should** be able to be used for 
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
 - `llvm-tools-preview` is required to build the bootloader and can be installed using `rustup component add llvm-tools-preview`
 - all other dependencies will be fetched by cargo during the build process.

### Actually building

Hootux is built and run via the root crate which acts as a runner the built binaries can be found thi the `target` directory

 - `cargo build`          : will build the binaries normally without running

### Running

Hootux comes with a runner for running with qemu-system-x86_64 which can run bia the following commands.
- `cargo run -- --uefi`  : will boot hootux in qemu using ovmf firmware
- `cargo run -- --bios`  : will boot hootux in qemu using legacy boot
- `cargo run -- --help ` : will provide a help message with extra arguments and their usages

Booting with uefi firmware requires the `runcfg.toml` to be set correctly. It requires at the very least to be pointed 
to a working uefi firmware image at the key `uefi.bios` This project will not provide a working firmware. I also 
recommend setting an `uefi.vars` a efivars file. 

## TODO

This part is mostly for my own sanity to try to manage what I want to do and to lay out the things I need to to to
achieve them.

 - Clean up memory module
   - Move allocator into mem
     - that's where it should be
 - AHCI driver
   - requires proper scheduling
     - ~~requires somewhat accurate timer~~ 
       - ~~use apic timer~~
         - ~~figure out how fast apic timer is~~
   - pci-e interface
     - preferably backward compatible
 - Enhance Allocator
   - Add optimizations where possible
   - Allow allocations greater than 4Mib
 - BASIC shell
   - what why? 
   - because it's my kernel
   - do after AHCI so it can access sata drives
 - Fix and write tests
   - because I really need to
 - Add gdbstub
 - Multiprocessing
   - NUMA?
   - thread local allocators?
    