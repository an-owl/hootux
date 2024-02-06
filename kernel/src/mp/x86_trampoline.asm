%define RQ_PAT 1
%define RQ_ROOT_PAGE_TABLE 2
%define RQ_LONG_MODE_STACK 3
%define RQ_RUST_ENTRY_POINT 4
%define RQ_TLS 5

%define gdt_ptr _trampoline_data + 0
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
    lgdt [cs:0x100]
    mov eax,cr0
    or eax,1
    mov cr0,eax
    jmp 8:0x20 ; set protected mode
    
    bits 32
    align 32
set_stack:
    mov ax,16
    mov ds,ax
    mov ss,ax ; temporary stack for init

    mov esp,4096 ; set stack pointer
    
set_longmode:
    mov esi, RQ_PAT ; get PAT values
    call request_32 ; calls are absolute, but near within the segment
    mov ecx,0x277
    ; ignore esi, read result manually
    mov dword eax,[rx] ; do these need to be DS:[..]?
    mov dword edx,[rx+4]
    wrmsr ; set PAT

    mov esi,RQ_ROOT_PAGE_TABLE
    call request_32 ; request top level page table
    mov cr3,edi

    mov ecx,0xC0000080
    rdmsr
    or eax,1 << 8
    wrmsr ; now in IA-3e compatibility mode

    mov esi,RQ_LONG_MODE_STACK
    call request_32 ; ignore result, move to rsp after jmp to long mode

    jmp 24:init_longmode

    ;takes esi is arg
    ;result returned in edi
    ;clobbers ebx
request_32:
    mov dword [tx],esi
    xor ebx,ebx
rq_spin_32:
    ; tx will be set to 0 by bsp when data is ready
    lock cmpxchg dword [tx],ebx
    pause
    je rq_spin_32,
    lfence
    mov dword edi,[rx]
    ret

    bits 64
init_longmode:
    mov qword rsi,[rx]
    mov ax,16
    mov ss,ax
    mov ds,ax

    mov esi,3
    call request_64
    mov rsp,rdi

    mov esi,RQ_TLS ; requests and loads the thread local segment
    call request_64
    mov dword eax,[rel rx]
    mov dword edx, [rel rx+4]
    mov ecx,0xc0000100 ; uses fs_base MSR to avoid needing to use GDT
    wrmsr

    mov esi,4
    call request_64 ; request address for jumping to ap_startup

    jmp rdi

    ;takes esi is arg
    ;result returned in rdi
    ;clobbers ebx
request_64:
    mov dword [tx],esi
    xor ebx,ebx
rq_spin_64:
    ; tx will be set to 0 by bsp
    lock cmpxchg dword [tx],ebx
    pause
    je rq_spin_64,
    lfence
    mov qword rdi,[rx]
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