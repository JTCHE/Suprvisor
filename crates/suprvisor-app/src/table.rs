//! The Processes tab as data: sections, groups and trees, sort, filter, and the flat list of rows
//! the renderer draws. No Win32 here, so the logic is testable.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use suprvisor_core::Proc;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Section {
    Apps,
    Background,
    Windows,
}

impl Section {
    fn title(self) -> &'static str {
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

pub const COLS: [Col; 7] = [Col::Name, Col::Pid, Col::Cpu, Col::Ram, Col::Gpu, Col::Vram, Col::Disk];

impl Col {
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

/// Sums over a row's subtree.
#[derive(Clone, Copy, Default, Debug)]
pub struct Values {
    pub cpu: f32,
    pub ram: u64,
    pub gpu: f32,
    pub vram: u64,
    pub read: u64,
    pub write: u64,
}

impl Values {
    fn of(p: &Proc) -> Self {
        Self { cpu: p.cpu, ram: p.private_ws, gpu: p.gpu, vram: p.vram, read: p.io_read, write: p.io_write }
    }

    fn add(&mut self, o: &Values) {
        self.cpu += o.cpu;
        self.ram += o.ram;
        self.gpu = (self.gpu + o.gpu).min(100.0);
        self.vram += o.vram;
        self.read += o.read;
        self.write += o.write;
    }
}

pub struct Row {
    pub key: Key,
    pub depth: u8,
    pub label: String,
    /// Dim text after the label: window title, or member count.
    pub detail: String,
    pub pid: Option<u32>,
    /// A process of the row, for the icon and the file actions.
    pub rep: Option<usize>,
    pub expandable: bool,
    pub expanded: bool,
    pub v: Values,
    /// Every process in the subtree (indices into `Table::procs`).
    pub procs: Vec<usize>,
}

struct Node {
    key: Key,
    label: String,
    lower: String, // label in lowercase, for sorting and filtering
    detail: String,
    pid: Option<u32>,
    rep: Option<usize>,
    v: Values,
    procs: Vec<usize>,
    children: Vec<Node>,
}

pub struct Table {
    pub procs: Arc<Vec<Proc>>,
    pub titles: Arc<HashMap<u32, String>>,
    pub rows: Vec<Row>,
    pub sort: Col,
    pub desc: bool,
    pub grouped: bool, // Apps / Background / Windows sections
    pub filter: String,
    /// Rows whose expanded state differs from the default (sections open, the rest closed).
    pub toggled: HashSet<Key>,
    pub selected: Option<Key>,
    pub hover: Option<usize>,
    pub scroll: usize,
    windir: String,
}

fn stem(name: &str) -> &str {
    name.len().checked_sub(4).filter(|&n| name[n..].eq_ignore_ascii_case(".exe")).map_or(name, |n| &name[..n])
}

impl Table {
    pub fn new(sort: Col, desc: bool, grouped: bool) -> Self {
        let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()).to_lowercase() + "\\";
        Self {
            procs: Arc::default(),
            titles: Arc::default(),
            rows: Vec::new(),
            sort,
            desc,
            grouped,
            filter: String::new(),
            toggled: HashSet::new(),
            selected: None,
            hover: None,
            scroll: 0,
            windir,
        }
    }

    /// Click on a column header: sort by it, or flip the direction. Numbers start high, names start at A.
    pub fn sort_by(&mut self, c: Col) {
        if self.sort == c {
            self.desc = !self.desc;
        } else {
            (self.sort, self.desc) = (c, !matches!(c, Col::Name | Col::Pid));
        }
        self.rebuild();
    }

    pub fn toggle(&mut self, i: usize) {
        if self.rows.get(i).is_some_and(|r| r.expandable) {
            let k = self.rows[i].key.clone();
            if !self.toggled.remove(&k) {
                self.toggled.insert(k);
            }
            self.rebuild();
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        let k = self.selected.as_ref()?;
        self.rows.iter().position(|r| &r.key == k)
    }

    fn section(&self, members: &[usize]) -> Section {
        let p = &self.procs[members[0]];
        if members.iter().any(|&i| self.titles.contains_key(&self.procs[i].pid)) {
            Section::Apps
        } else if p.path.is_empty() || p.path.to_lowercase().starts_with(&self.windir) {
            // ponytail: processes no handle opens (anti-cheat, some services) have no path and land
            // here too. Their image path from the kernel would fix it.
            Section::Windows
        } else {
            Section::Background
        }
    }

    fn proc_node(&self, i: usize, kids: &HashMap<usize, Vec<usize>>) -> Node {
        let p = &self.procs[i];
        let children: Vec<Node> = kids.get(&i).into_iter().flatten().map(|&c| self.proc_node(c, kids)).collect();
        let mut v = Values::of(p);
        let mut procs = vec![i];
        for c in &children {
            v.add(&c.v);
            procs.extend(&c.procs);
        }
        let label = stem(&p.name).to_owned();
        Node {
            key: Key::Proc(p.pid, p.create_time),
            lower: label.to_lowercase(),
            label,
            detail: self.titles.get(&p.pid).cloned().unwrap_or_default(),
            pid: Some(p.pid),
            rep: Some(i),
            v,
            procs,
            children,
        }
    }

    fn parent_node(key: Key, label: String, detail: String, rep: Option<usize>, children: Vec<Node>) -> Node {
        let mut v = Values::default();
        let mut procs = Vec::new();
        for c in &children {
            v.add(&c.v);
            procs.extend(&c.procs);
        }
        Node { key, lower: label.to_lowercase(), label, detail, pid: None, rep, v, procs, children }
    }

    fn tree(&self) -> Vec<Node> {
        // Group by exe name, in a section.
        let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, p) in self.procs.iter().enumerate() {
            if p.pid != 0 {
                groups.entry(p.name.to_lowercase()).or_default().push(i);
            }
        }
        let by_pid: HashMap<u32, usize> = self.procs.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();
        let mut sections: HashMap<Option<Section>, Vec<Node>> = HashMap::new();
        for (name, members) in groups {
            // Inside a group, a process nests under its parent when the parent is in the group too.
            // The parent must be older: an exited parent's pid can be reused.
            let mut kids: HashMap<usize, Vec<usize>> = HashMap::new();
            let mut roots = Vec::new();
            for &i in &members {
                let p = &self.procs[i];
                match by_pid.get(&p.parent_pid) {
                    Some(&j) if j != i && self.procs[j].name.eq_ignore_ascii_case(&p.name) && self.procs[j].create_time < p.create_time => {
                        kids.entry(j).or_default().push(i)
                    }
                    _ => roots.push(i),
                }
            }
            let section = self.grouped.then(|| self.section(&members));
            let node = if members.len() == 1 {
                self.proc_node(members[0], &kids)
            } else {
                let roots = roots.iter().map(|&r| self.proc_node(r, &kids)).collect();
                let label = stem(&self.procs[members[0]].name).to_owned();
                Self::parent_node(Key::Group(section, name), label, format!("({})", members.len()), Some(members[0]), roots)
            };
            sections.entry(section).or_default().push(node);
        }
        if !self.grouped {
            return sections.remove(&None).unwrap_or_default();
        }
        [Section::Apps, Section::Background, Section::Windows]
            .into_iter()
            .filter_map(|s| {
                let groups = sections.remove(&Some(s))?;
                let n = groups.len();
                Some(Self::parent_node(Key::Section(s), s.title().to_owned(), format!("({n})"), None, groups))
            })
            .collect()
    }

    fn cmp(&self, a: &Node, b: &Node) -> Ordering {
        let name = || a.lower.cmp(&b.lower);
        let o = match self.sort {
            Col::Name => name(),
            Col::Pid => a.pid.cmp(&b.pid),
            Col::Cpu => a.v.cpu.total_cmp(&b.v.cpu),
            Col::Ram => a.v.ram.cmp(&b.v.ram),
            Col::Gpu => a.v.gpu.total_cmp(&b.v.gpu),
            Col::Vram => a.v.vram.cmp(&b.v.vram),
            Col::Disk => (a.v.read + a.v.write).cmp(&(b.v.read + b.v.write)),
        };
        let o = if self.desc { o.reverse() } else { o };
        o.then_with(name).then(a.pid.cmp(&b.pid))
    }

    fn sort_nodes(&self, nodes: &mut [Node], top: bool) {
        if !(top && self.grouped) {
            nodes.sort_by(|a, b| self.cmp(a, b)); // sections keep their order
        }
        for n in nodes {
            self.sort_nodes(&mut n.children, false);
        }
    }

    /// Drops nodes that do not match the filter and have no matching descendant.
    /// Returns whether the node stays, and opens it when only a descendant matches.
    fn keep(&self, n: &mut Node, f: &str, open: &mut HashSet<Key>) -> bool {
        let pid = n.pid.is_some_and(|p| p.to_string().starts_with(f));
        if !matches!(n.key, Key::Section(_)) && (pid || n.lower.contains(f) || n.detail.to_lowercase().contains(f)) {
            return true;
        }
        n.children.retain_mut(|c| self.keep(c, f, open));
        if n.children.is_empty() {
            return false;
        }
        open.insert(n.key.clone());
        true
    }

    /// Recomputes the rows from the processes, sort, filter and expanded state. Keeps the selection.
    pub fn rebuild(&mut self) {
        let mut nodes = self.tree();
        let mut open = HashSet::new();
        let f = self.filter.to_lowercase();
        if !f.is_empty() {
            nodes.retain_mut(|n| self.keep(n, &f, &mut open));
        }
        self.sort_nodes(&mut nodes, true);
        let mut rows = Vec::with_capacity(self.rows.len());
        self.flatten(nodes, 0, &open, &mut rows);
        self.rows = rows;
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
    }

    fn flatten(&self, nodes: Vec<Node>, depth: u8, open: &HashSet<Key>, out: &mut Vec<Row>) {
        for n in nodes {
            let expandable = !n.children.is_empty();
            let expanded = expandable && (open.contains(&n.key) || (matches!(n.key, Key::Section(_)) != self.toggled.contains(&n.key)));
            out.push(Row {
                key: n.key,
                depth,
                label: n.label,
                detail: n.detail,
                pid: n.pid,
                rep: n.rep,
                expandable,
                expanded,
                v: n.v,
                procs: n.procs,
            });
            if expanded {
                self.flatten(n.children, depth + 1, open, out);
            }
        }
    }

    /// (pid, create time) of every process under `rows[range]`: what the collector refreshes fast.
    /// Section rows show no numbers, so their processes are left out.
    pub fn keys(&self, range: std::ops::Range<usize>) -> Vec<(u32, i64)> {
        let rows = &self.rows[range.start.min(self.rows.len())..range.end.min(self.rows.len())];
        rows.iter().filter(|r| !matches!(r.key, Key::Section(_))).flat_map(|r| &r.procs).map(|&i| (self.procs[i].pid, self.procs[i].create_time)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, name: &str, path: &str, cpu: f32) -> Proc {
        Proc { pid, parent_pid: parent, name: name.into(), path: path.into(), create_time: pid as i64, cpu, ..Default::default() }
    }

    fn table(procs: Vec<Proc>, titles: &[(u32, &str)]) -> Table {
        let mut t = Table::new(Col::Name, false, true);
        t.windir = r"c:\windows\".into();
        t.procs = Arc::new(procs);
        t.titles = Arc::new(titles.iter().map(|&(p, s)| (p, s.to_owned())).collect());
        t.rebuild();
        t
    }

    fn labels(t: &Table) -> Vec<String> {
        t.rows.iter().map(|r| format!("{}{} {}", " ".repeat(r.depth as usize), r.label, r.detail).trim_end().to_owned()).collect()
    }

    #[test]
    fn sections_groups_trees_sort_filter() {
        let mut t = table(
            vec![
                p(0, 0, "Idle", "", 50.0),
                p(4, 0, "System", "", 1.0),
                p(10, 4, "svchost.exe", r"C:\Windows\System32\svchost.exe", 2.0),
                p(11, 4, "svchost.exe", r"C:\Windows\System32\svchost.exe", 3.0),
                p(20, 1, "Code.exe", r"C:\VS\Code.exe", 1.0),
                p(21, 20, "Code.exe", r"C:\VS\Code.exe", 4.0),
                p(22, 21, "Code.exe", r"C:\VS\Code.exe", 5.0),
                p(30, 1, "sync.exe", r"C:\Sync\sync.exe", 0.5),
            ],
            &[(20, "main.rs - Suprvisor")],
        );
        assert_eq!(labels(&t), ["Apps (1)", " Code (3)", "Background processes (1)", " sync", "Windows processes (2)", " svchost (2)", " System"]);
        let code = &t.rows[1];
        assert!((code.v.cpu - 10.0).abs() < 1e-3 && code.procs.len() == 3, "group sums its members");

        t.toggle(1); // open Code: the window instance, then its child tree
        t.toggle(2);
        t.toggle(3);
        assert_eq!(labels(&t)[1..5], [" Code (3)", "  Code main.rs - Suprvisor", "   Code", "    Code"]);
        assert!((t.rows[3].v.cpu - 9.0).abs() < 1e-3, "a process row sums its subtree");

        t.sort_by(Col::Cpu);
        assert_eq!(labels(&t)[t.rows.len() - 2..], [" svchost (2)", " System"], "sections keep their order; CPU sorts inside");

        t.filter = "11".into(); // a pid: its group opens to show it
        t.rebuild();
        assert_eq!(labels(&t), ["Windows processes (2)", " svchost (2)", "  svchost"]);
        t.filter = "suprvisor".into(); // a window title
        t.rebuild();
        assert_eq!(labels(&t)[..3], ["Apps (1)", " Code (3)", "  Code main.rs - Suprvisor"]);

        t.grouped = false;
        t.filter.clear();
        t.rebuild();
        assert_eq!(t.rows.len(), 4, "flat: one row per group");
        assert_eq!(t.keys(0..1).len(), 3);
        t.grouped = true;
        t.rebuild();
        assert_eq!(t.keys(0..1).len(), 0, "a section row needs no refresh");
    }
}
