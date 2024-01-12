 segment .hootux_ap_trampoline

 global _trampoline
 global _bsp_xfer_sem
     bits 16
     align 4096
 _trampoline:
    cli
    cld
    jmp get_gdt
_bsp_xfer_sem:
    rx: dq 0,
    tx: dw 0,
get_gdt:
    ;fetch GDT address
    ;GDT address will be written here before AP is started
    xor edi,edi
    xchg dword edi,[rx]
    
    lgdt [edi]
    mov eax,cr0
    or eax,1
    mov cr0,eax
    jmp 8:set_stack
    
    bits 32
    align 32
set_stack:
    mov ax,16
    mov ds,ax
    mov ss,ax ; temporary stack for init
    mov dword [tx],1
    xor edi,edi
spin_for_stack:
    lock cmpxchg dword [rx],edi
    pause
    je spin_for_stack
    mov esp,edi
    
set_longmode:
    mov ecx,0xC0000080
    rdmsr
    or eax,1 << 8
    wrmsr ; efer.lme is set
    
    mov esi,2 ; get PAT values
    call request
    mov ecx,0x277
    mov dword eax,[rx]
    mov dword edx,[rx+4]
    wrmsr ; set PAT
    
    mov esi,3
    call request ; request top level page table
    mov dword eax,[rx]
    mov cr3,eax
    
    mov esi,4
    call request ; request address for jumping to ap_startup


    ;takes esi is arg 
    ;result returned in edi
request: 
    mov dword [tx],esi
    xor edx,edx
rq_spin:
    lock cmpxchg dword [rx],edx
    pause
    je rq_spin,
    ; tx will be set to 0 by bsp
    ret