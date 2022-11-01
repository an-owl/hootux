#Hootux

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

##Building

###Dependancies

 - Rust is required to build this project. Installation instructions can be found [here](https://rustup.rs/)
 - `rust-src` is required to build the kernel binary and can be installed using `rustup component add rust-src`
 - `llvm-tools-preview` is required to build the bootloader and can be installed using `rustup component add llvm-tools-preview`
 - all other dependencies will be fetched by cargo during the build process.

###Actually building

Convenience functions are provided to build image and run the kernel and may not work on all systems.

 - `cargo krun`  : Will build the kernel images and run it using qemu however this may not work on all systems and is not recommended
 - `cargo image` : Will build the kernel images into `./target/x86_64-owl-os-build`
 - `cargo kbuild`: Will only build the kernel binary without imaging it

##TODO

This part is mostly for my own sanity to try to manage what I want to do and to lay out the things I need to to to
achieve them.

 - Clean up memory module
   - Move allocator into mem
     - that's where it should be
   - unify metrics in page_table_tree.rs
     - many things in there use addresses and pages interchangeably they should all use `Page<Size4kib>`
     - unify functions too, there are a lot in there that do very similar things and should be condensed
 - AHCI driver
   - requires proper scheduling
     - --requires somewhat accurate timer--
       - --use apic timer--
         - --figure out how fast apic timer is--
   - pci-e interface
     - preferably backward compatible
 - Switch to Buddy allocator
   - because its soo much better than the crap I have now
 - BASIC shell
   - what why? 
   - because it's my kernel
   - do after AHCI so it can access sata drives
 - Fix and write tests
   - because I really need to
    