//! The process tree: sections, groups by exe, parent and child processes, sort.

use std::cmp::Ordering;
use std::collections::HashMap;

use suprvisor_core::Proc;

use super::*;

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
    pub(super) fn of(p: &Proc) -> Self {
        Self { cpu: p.cpu, ram: p.private_ws, gpu: p.gpu, vram: p.vram, read: p.io_read, write: p.io_write }
    }

    pub(super) fn add(&mut self, o: &Values) {
        self.cpu += o.cpu;
        self.ram += o.ram;
        self.gpu = (self.gpu + o.gpu).min(100.0);
        self.vram += o.vram;
        self.read += o.read;
        self.write += o.write;
    }
}

pub(super) struct Node {
    pub(super) key: Key,
    pub(super) label: String,
    pub(super) lower: String, // label in lowercase, for sorting and filtering
    pub(super) detail: String,
    pub(super) pid: Option<u32>,
    pub(super) rep: Option<usize>,
    pub(super) v: Values,
    pub(super) procs: Vec<usize>,
    pub(super) children: Vec<Node>,
}

pub(super) fn stem(name: &str) -> &str {
    name.len().checked_sub(4).filter(|&n| name[n..].eq_ignore_ascii_case(".exe")).map_or(name, |n| &name[..n])
}

impl Table {
    pub(super) fn section(&self, members: &[usize]) -> Section {
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

    pub(super) fn proc_node(&self, i: usize, kids: &HashMap<usize, Vec<usize>>) -> Node {
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

    pub(super) fn parent_node(key: Key, label: String, detail: String, rep: Option<usize>, children: Vec<Node>) -> Node {
        let mut v = Values::default();
        let mut procs = Vec::new();
        for c in &children {
            v.add(&c.v);
            procs.extend(&c.procs);
        }
        Node { key, lower: label.to_lowercase(), label, detail, pid: None, rep, v, procs, children }
    }

    pub(super) fn tree(&self) -> Vec<Node> {
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

    pub(super) fn cmp(&self, a: &Node, b: &Node) -> Ordering {
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

    pub(super) fn sort_nodes(&self, nodes: &mut [Node], top: bool) {
        if !(top && self.grouped) {
            nodes.sort_by(|a, b| self.cmp(a, b)); // sections keep their order
        }
        for n in nodes {
            self.sort_nodes(&mut n.children, false);
        }
    }
}
