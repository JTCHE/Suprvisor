//! The table's columns, sections and row keys.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Section {
    Apps,
    Background,
    Windows,
}

impl Section {
    pub(super) fn title(self) -> &'static str {
        match self {
            Section::Apps => "Apps",
            Section::Background => "Background processes",
            Section::Windows => "Windows processes",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Col {
    Name,
    Pid,
    Cpu,
    Ram,
    Gpu,
    Vram,
    Disk,
}

pub const COLS: [Col; 7] = [Col::Name, Col::Cpu, Col::Ram, Col::Gpu, Col::Vram, Col::Disk, Col::Pid];

/// The columns after Name, in their default order. Widths, order and visibility are kept per column
/// in this order (`Col::num`).
pub const NUMS: [Col; 6] = [Col::Cpu, Col::Ram, Col::Gpu, Col::Vram, Col::Disk, Col::Pid];

impl Col {
    /// Index in `NUMS`; None for Name.
    pub fn num(self) -> Option<usize> {
        NUMS.iter().position(|&c| c == self)
    }

    pub fn label(self) -> &'static str {
        match self {
            Col::Name => "Name",
            Col::Pid => "PID",
            Col::Cpu => "CPU",
            Col::Ram => "Memory",
            Col::Gpu => "GPU",
            Col::Vram => "VRAM",
            Col::Disk => "Disk r / w",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Col::Name => "name",
            Col::Pid => "pid",
            Col::Cpu => "cpu",
            Col::Ram => "ram",
            Col::Gpu => "gpu",
            Col::Vram => "vram",
            Col::Disk => "disk",
        }
    }

    pub fn from_id(s: &str) -> Option<Col> {
        COLS.into_iter().find(|c| c.id() == s)
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    Section(Section),
    Group(Option<Section>, String), // lowercase exe name
    Proc(u32, i64),                 // pid, create time
}
