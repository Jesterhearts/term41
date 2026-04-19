use pty_pipe41::ForegroundProcessSet;
use pty_pipe41::ForegroundProgram;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FeaturePermissions {
    pub macros: ProgramAllowlist,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProgramAllowlist {
    #[default]
    DenyAll,
    AllowAll,
    Programs(Vec<String>),
}

impl ProgramAllowlist {
    pub fn allows_programs(
        &self,
        processes: Option<&ForegroundProcessSet>,
    ) -> bool {
        let Some(processes) = processes else {
            return false;
        };
        if processes.is_empty() {
            return false;
        }

        trace!("Checking allowlist {self:?} against foreground processes {processes:?}");

        match self {
            Self::DenyAll => false,
            Self::AllowAll => true,
            Self::Programs(entries) => processes
                .programs
                .iter()
                .all(|program| entries.iter().any(|entry| program_matches(entry, program))),
        }
    }
}

fn program_matches(
    entry: &str,
    program: &ForegroundProgram,
) -> bool {
    trace!("Matching program entry {entry:?} against foreground program {program:?}");
    if entry.contains('/') {
        program.exe_path == std::path::Path::new(entry)
    } else {
        program.exe_name == entry
    }
}
