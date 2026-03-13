// Rust guideline compliant 2026-02-21
//! Raw ptrace stop types produced by the ptrace thread.
//!
//! These are serializable so they can be recorded for replay testing
//! without requiring a live ptrace session.

use serde::{Deserialize, Serialize};

/// Serialization helper for `nix::unistd::Pid`, which has no serde impl.
mod pid_serde {
    use nix::unistd::Pid;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(pid: &Pid, s: S) -> Result<S::Ok, S::Error> {
        pid.as_raw().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Pid, D::Error> {
        let raw = i32::deserialize(d)?;
        Ok(Pid::from_raw(raw))
    }
}

/// Six general-purpose syscall arguments passed at entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyscallArgs {
    pub arg0: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
    pub arg4: u64,
    pub arg5: u64,
}

impl SyscallArgs {
    /// Construct from a flat array (aarch64: regs[0..5]).
    pub fn from_array(args: [u64; 6]) -> Self {
        Self {
            arg0: args[0],
            arg1: args[1],
            arg2: args[2],
            arg3: args[3],
            arg4: args[4],
            arg5: args[5],
        }
    }
}

/// Discriminated stop event delivered from the ptrace thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StopType {
    /// Tracee reached a syscall entry point via seccomp.
    SyscallEntry {
        syscall_nr: u64,
        args: SyscallArgs,
    },
    /// Tracee returned from a syscall.
    SyscallExit {
        syscall_nr: u64,
        return_value: i64,
    },
    /// A fork/vfork/clone created a new child.
    Fork {
        #[serde(with = "pid_serde")]
        parent: nix::unistd::Pid,
        #[serde(with = "pid_serde")]
        child: nix::unistd::Pid,
    },
    /// A program replacement (execve) completed.
    Exec {
        #[serde(with = "pid_serde")]
        pid: nix::unistd::Pid,
    },
    /// A process exited normally or via signal.
    Exit {
        #[serde(with = "pid_serde")]
        pid: nix::unistd::Pid,
        exit_code: i32,
    },
    /// A signal was delivered to the tracee.
    Signal {
        #[serde(with = "pid_serde")]
        pid: nix::unistd::Pid,
        signal: i32,
    },
    /// An unrecognized or unhandled ptrace stop.
    Unknown,
}

/// A ptrace stop bundled with the pid it arrived on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawSyscallStop {
    #[serde(with = "pid_serde")]
    pub pid: nix::unistd::Pid,
    pub stop_type: StopType,
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::Pid;

    #[test]
    fn syscall_args_round_trip() {
        let args = SyscallArgs::from_array([1, 2, 3, 4, 5, 6]);
        let json = serde_json::to_string(&args).unwrap();
        let back: SyscallArgs = serde_json::from_str(&json).unwrap();
        assert_eq!(args, back);
    }

    #[test]
    fn raw_stop_serde_round_trip() {
        let stop = RawSyscallStop {
            pid: Pid::from_raw(42),
            stop_type: StopType::SyscallEntry {
                syscall_nr: 1,
                args: SyscallArgs::from_array([0; 6]),
            },
        };
        let json = serde_json::to_string(&stop).unwrap();
        let back: RawSyscallStop = serde_json::from_str(&json).unwrap();
        assert_eq!(stop, back);
    }

    #[test]
    fn fork_stop_serde() {
        let stop = RawSyscallStop {
            pid: Pid::from_raw(1),
            stop_type: StopType::Fork {
                parent: Pid::from_raw(1),
                child: Pid::from_raw(2),
            },
        };
        let json = serde_json::to_string(&stop).unwrap();
        let back: RawSyscallStop = serde_json::from_str(&json).unwrap();
        assert_eq!(stop, back);
    }
}
