// Rust guideline compliant 2026-02-21
//! Raw BPF program builder for seccomp filters.
//!
//! Constructs a classic BPF (cBPF) program that inspects `seccomp_data`
//! and returns the appropriate action per syscall number. Only supports
//! x86_64 (`AUDIT_ARCH_X86_64`).

/// Describes what action the filter should take for a syscall.
#[derive(Debug, Clone, Copy)]
pub enum SyscallAction {
    /// Generate a ptrace stop (`SECCOMP_RET_TRACE`).
    Trace(u64),
    /// Return `ENOSYS` to the caller (`SECCOMP_RET_ERRNO`).
    Errno(u64),
}

impl SyscallAction {
    fn syscall_nr(self) -> u64 {
        match self {
            Self::Trace(nr) | Self::Errno(nr) => nr,
        }
    }

    fn ret_value(self) -> u32 {
        match self {
            Self::Trace(_) => SECCOMP_RET_TRACE,
            Self::Errno(_) => SECCOMP_RET_ERRNO | ENOSYS,
        }
    }
}

/// Offset of `seccomp_data.arch` (u32 at byte 4).
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;

/// Offset of `seccomp_data.nr` (i32 at byte 0 of `seccomp_data`).
const SECCOMP_DATA_NR_OFFSET: u32 = 0;

/// `AUDIT_ARCH_X86_64` = `EM_X86_64 | __AUDIT_ARCH_64BIT | __AUDIT_ARCH_LE`
const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;

const SECCOMP_RET_ALLOW: u32 = 0x7FFF_0000;
const SECCOMP_RET_TRACE: u32 = 0x7FF0_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;

/// `ENOSYS` on x86_64 Linux.
const ENOSYS: u32 = 38;

/// Builds a BPF program from a list of syscall actions.
///
/// Program layout:
/// - [0] load `seccomp_data.arch`
/// - [1] jeq x86_64 -> skip kill
/// - [2] ret KILL_PROCESS
/// - [3] load `seccomp_data.nr`
/// - [4..4+N) one JEQ per action, jumping to its RET instruction
/// - [4+N] ret ALLOW (default)
/// - [4+N+1..4+2N+1) one RET per action
///
/// Each JEQ at index `4+i` targets the RET at index `4+N+1+i`.
/// BPF jump offset = target - source - 1 = N (constant for all actions).
///
/// # Panics
///
/// Panics if the resulting program exceeds the BPF maximum of 4096
/// instructions (would require ~2045 actions, far beyond our ~58).
pub fn build_filter_program(actions: &[SyscallAction]) -> Vec<libc::sock_filter> {
    let num_actions = actions.len();
    // 4 header + N jeqs + 1 default allow + N rets
    let total = 4 + num_actions + 1 + num_actions;
    let mut insns: Vec<libc::sock_filter> = Vec::with_capacity(total);

    // --- Header: validate architecture ---
    insns.push(bpf_stmt(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_ARCH_OFFSET));
    insns.push(bpf_jump(
        BPF_JMP | BPF_JEQ | BPF_K,
        AUDIT_ARCH_X86_64,
        1,
        0,
    ));
    // Wrong architecture: kill the process to prevent bypass.
    insns.push(bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));

    // --- Load syscall number ---
    insns.push(bpf_stmt(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFFSET));

    // --- Per-syscall JEQ instructions ---
    // All jump to offset `num_actions` from next instruction, which lands
    // on the matching RET instruction past the default ALLOW.
    for action in actions {
        let nr = u32::try_from(action.syscall_nr())
            .expect("x86_64 syscall numbers fit in u32");
        insns.push(bpf_jump(
            BPF_JMP | BPF_JEQ | BPF_K,
            nr,
            num_actions as u8,
            0,
        ));
    }

    // --- Default: allow unmatched syscalls at native speed ---
    insns.push(bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));

    // --- Per-action RET instructions ---
    for action in actions {
        insns.push(bpf_stmt(BPF_RET | BPF_K, action.ret_value()));
    }

    assert!(
        insns.len() <= 4096,
        "BPF program has {} instructions, max is 4096",
        insns.len()
    );

    insns
}

// --- BPF instruction constants ---
// These mirror <linux/bpf_common.h> and <linux/filter.h>.

const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

/// Creates a BPF statement (no jump targets).
fn bpf_stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

/// Creates a BPF jump instruction.
fn bpf_jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_actions_produce_minimal_program() {
        let program = build_filter_program(&[]);
        // 3 header + 1 load nr + 1 default allow = 5
        assert_eq!(program.len(), 5);
    }

    #[test]
    fn single_trace_action() {
        let program = build_filter_program(&[SyscallAction::Trace(2)]);
        // 4 header + 1 jeq + 1 allow + 1 ret = 7
        assert_eq!(program.len(), 7);

        // JEQ at index 4 should jump offset 1 (num_actions) to index 6
        assert_eq!(program[4].jt, 1);
        assert_eq!(program[4].k, 2); // syscall nr

        // Index 5: default allow
        assert_eq!(program[5].k, SECCOMP_RET_ALLOW);

        // Index 6: ret trace
        assert_eq!(program[6].k, SECCOMP_RET_TRACE);
    }

    #[test]
    fn single_errno_action() {
        let program = build_filter_program(&[SyscallAction::Errno(425)]);
        assert_eq!(program.len(), 7);
        assert_eq!(program[6].k, SECCOMP_RET_ERRNO | ENOSYS);
    }

    #[test]
    fn jump_offsets_target_correct_ret() {
        let actions = vec![
            SyscallAction::Trace(0),
            SyscallAction::Trace(1),
            SyscallAction::Errno(425),
        ];
        let program = build_filter_program(&actions);

        // Layout:
        // 0: load arch
        // 1: jeq arch
        // 2: kill
        // 3: load nr
        // 4: jeq(0)  -> jt=3, target = 4+1+3 = 8
        // 5: jeq(1)  -> jt=3, target = 5+1+3 = 9
        // 6: jeq(425) -> jt=3, target = 6+1+3 = 10
        // 7: ret allow
        // 8: ret trace (action 0)
        // 9: ret trace (action 1)
        // 10: ret errno (action 2)

        assert_eq!(program.len(), 11);

        // All JEQ offsets equal num_actions = 3
        assert_eq!(program[4].jt, 3);
        assert_eq!(program[5].jt, 3);
        assert_eq!(program[6].jt, 3);

        // Verify targets
        assert_eq!(program[8].k, SECCOMP_RET_TRACE);
        assert_eq!(program[9].k, SECCOMP_RET_TRACE);
        assert_eq!(program[10].k, SECCOMP_RET_ERRNO | ENOSYS);

        // Default allow
        assert_eq!(program[7].k, SECCOMP_RET_ALLOW);
    }

    #[test]
    fn arch_check_kills_on_mismatch() {
        let program = build_filter_program(&[]);
        // Index 1: JEQ for arch, jt=1 (skip kill), jf=0 (fall through)
        assert_eq!(program[1].jt, 1);
        assert_eq!(program[1].jf, 0);
        assert_eq!(program[1].k, AUDIT_ARCH_X86_64);
        // Index 2: KILL_PROCESS
        assert_eq!(program[2].k, SECCOMP_RET_KILL_PROCESS);
    }
}
