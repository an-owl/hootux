%define RQ_PAT 1
%define RQ_ROOT_PAGE_TABLE 2
%define RQ_LONG_MODE_STACK 3
%define RQ_RUST_ENTRY_POINT 4
%define RQ_TLS 5

%define data_offset _trampoline_data - $
%define gdt_ptr data_offset
%define rx _trampoline_data + 80
%define tx _trampoline_data + 88
segment .hootux_ap_trampoline

global _trampoline
global _trampoline_data
    bits 16
    align 4096
_trampoline:
    cli
    cld

; lgdt & ljump need to be resolved manually (until i figure out why the bootloader wont co-operate)
get_gdt:
    ;fetch GDT address
    ;GDT address will be written here before AP is started
    lgdt [cs:_trampoline_data - _trampoline]
    mov eax,cr0
    or eax,1 ; set cr0.pm
    mov cr0,eax
    jmp 8:set_stack - _trampoline ; set protected mode
    
    bits 32
    align 32
set_stack:
    mov esp,4096
    sub esp,_trampoline_data - _trampoline ; This will set the stack pointer to the last byte in the page.
    mov ax,16
    mov ds,ax
    mov ss,ax ; temporary stack for init

set_longmode:
    mov esi, RQ_PAT ; get PAT values
    call request_32 ; Why are these addresses resolved correctly? (well incorrectly I suppose)
    mov ecx,0x277
    ; ignore esi, read result manually
    mov dword eax,[80]
    mov dword edx,[84]
    wrmsr ; set PAT

    mov esi,RQ_ROOT_PAGE_TABLE
    call request_32 ; request top level page table
    mov cr3,edi

    mov ecx,0xC0000080 ; efer
    rdmsr
    or eax,0x9 << 8 ; set efer.lme and efer.nxe
    ; XD bit isn't ignored if this efer.nxe is clear, no instead it page faults
    wrmsr ; now in IA-3e compatibility mode

    ; Set cr4.pvi cr4.pae cr4.mce cr4.
    mov edi,0x668
    mov cr4,edi

    ; enable paging
    mov edi,cr0
    or edi,1<<31
    mov cr0,edi

    mov esi,RQ_LONG_MODE_STACK
    call request_32 ; ignore result, move to rsp after jmp to long mode

    jmp 24:init_longmode - _trampoline

    ;takes esi is arg
    ;esi is not modified
    ;result returned in edi
    ;object comments are wrong here, it doesnt know that ds:0 and cs:0 are different
request_32:
    mov dword [88],esi
rq_spin_32:
    ; tx will be set to 0 by bsp when data is ready
    cmp dword [88],esi
    pause
    je rq_spin_32,
    lfence
    mov dword edi,[80]
    ret

    bits 64
init_longmode:
    mov ax,32
    mov ss,ax
    mov ds,ax
    lea r9,[rel rx] ; r9 is now &rx
    lea r10,[rel tx] ; r10 is &tx
    mov qword rsp,[r9] ; load kernel stack pointer (This will be the base from here on)

    mov esi,RQ_TLS ; requests and loads the thread local segment
    call request_64
    mov dword eax,[r9]
    mov dword edx, [r9 + 4]
    mov ecx,0xc0000100 ; uses fs_base MSR to avoid needing to use GDT
    wrmsr

    mov esi,RQ_RUST_ENTRY_POINT
    call request_64 ; request address for jumping to ap_startup

    jmp rdi

    ;takes esi is arg
    ;result returned in rdi
request_64:
    mov dword [r10],esi
rq_spin_64:
    ; tx will be set to 0 by bsp
    cmp dword [r10],esi
    pause
    je rq_spin_64,
    lfence
    mov qword rdi,[r9]
    ret

    align 8
_trampoline_data: ; trick linker into thinking this is code
    nop ; use this as magic number [90, 0f, 0b, cc]
    ud2
    int3

;_gdt_ptr:
;    len dw 64,
;    gdt_ptr dd 0,
;; written into by BSP
;_initial_gdt:
;    gdt times 9 dq 0
;_bsp_xfer_sem:
;    rx: dq 0,
;    tx: dw 0,