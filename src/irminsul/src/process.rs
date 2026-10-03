//! Small process helpers.
//!
//! Two uses, both by process id or name from a single snapshot:
//!
//! - tell the app whether the game is running, so "start the game" and "the
//!   game is running but the handshake was missed" can be told apart;
//! - notice that the RyukinLedger app has exited, so the core does not linger
//!   as an elevated process nobody can stop.

/// Genshin Impact executable names: the Chinese client and the global client.
const GAME_PROCESSES: [&str; 2] = ["YuanShen.exe", "GenshinImpact.exe"];

/// Whether the game is currently running.
pub fn game_running() -> bool {
    running_processes()
        .iter()
        .any(|process| GAME_PROCESSES.iter().any(|game| process.name.eq_ignore_ascii_case(game)))
}

/// Whether a process with this id is still running.
pub fn is_running(pid: u32) -> bool {
    pid != 0 && running_processes().iter().any(|process| process.pid == pid)
}

pub struct ProcessEntry {
    pub pid: u32,
    pub name: String,
}

#[cfg(windows)]
fn running_processes() -> Vec<ProcessEntry> {
    use windows::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };

    let mut processes = Vec::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return processes;
        };
        if snapshot == INVALID_HANDLE_VALUE {
            return processes;
        }

        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                if end > 0 {
                    processes.push(ProcessEntry {
                        pid: entry.th32ProcessID,
                        name: String::from_utf16_lossy(&entry.szExeFile[..end]),
                    });
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _ = CloseHandle(snapshot);
    }
    processes
}

#[cfg(not(windows))]
fn running_processes() -> Vec<ProcessEntry> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check has to work without the game running and must not report a
    /// false positive for an arbitrary process.
    #[test]
    fn does_not_claim_the_game_is_running_unless_it_is() {
        let processes = running_processes();
        assert!(!processes.is_empty(), "process enumeration returned nothing");
        assert!(processes.iter().any(|process| process.name.contains(".exe")));
        assert_eq!(
            game_running(),
            processes
                .iter()
                .any(|process| GAME_PROCESSES.iter().any(|game| process.name.eq_ignore_ascii_case(game)))
        );
    }

    /// The watchdog the app relies on: this very process must be found, and a
    /// pid that cannot be in use must not be.
    #[test]
    fn finds_live_processes_by_id() {
        assert!(is_running(std::process::id()), "could not find the test process itself");
        assert!(!is_running(0));
        // A pid above the maximum is never valid on Windows.
        assert!(!is_running(u32::MAX - 1));
    }
}
