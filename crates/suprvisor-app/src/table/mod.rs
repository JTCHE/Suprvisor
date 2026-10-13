//! The Processes tab as data: sections, groups and trees, sort, filter, and the flat list of rows
//! the renderer draws. No Win32 here, so the logic is testable.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use suprvisor_core::Proc;

mod columns;
mod tree;

pub use columns::*;
pub use tree::Values;
use tree::Node;

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
    /// The cursor row: arrow keys move from it, Shift extends from `anchor` to it.
    pub selected: Option<Key>,
    pub anchor: Option<Key>,
    /// Every selected row. End task acts on all of them.
    pub marked: HashSet<Key>,
    pub hover: Option<usize>,
    /// First row on screen, in rows; fractional while a smooth scroll moves.
    pub scroll: f32,
    windir: String,
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
            anchor: None,
            marked: HashSet::new(),
            hover: None,
            scroll: 0.0,
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

    pub fn index_of(&self, k: &Option<Key>) -> Option<usize> {
        let k = k.as_ref()?;
        self.rows.iter().position(|r| &r.key == k)
    }

    /// Rows `a..=b` (either order) become the selection, on top of `base`.
    pub fn mark_range(&mut self, a: usize, b: usize, base: &HashSet<Key>) {
        let (a, b) = (a.min(b), a.max(b).min(self.rows.len().saturating_sub(1)));
        self.marked = base.clone();
        self.marked.extend(self.rows.get(a..=b).into_iter().flatten().map(|r| r.key.clone()));
    }

    /// Selected rows, top to bottom.
    pub fn marked_rows(&self) -> Vec<usize> {
        (0..self.rows.len()).filter(|&i| self.marked.contains(&self.rows[i].key)).collect()
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
        // A selected row that is gone (ended, filtered out, folded away) leaves the selection, so
        // End task never acts on a row that is not on screen.
        let keys: HashSet<&Key> = self.rows.iter().map(|r| &r.key).collect();
        self.marked.retain(|k| keys.contains(k));
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1) as f32);
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

        let none = HashSet::new();
        t.mark_range(3, 1, &none);
        assert_eq!(t.marked_rows(), [1, 2, 3], "a drag up selects the same rows as a drag down");
        t.toggle(1); // fold Code: its members leave the selection with their rows
        assert!(t.marked_rows().iter().all(|&i| i < t.rows.len()));
        t.filter = "sync".into();
        t.rebuild();
        assert!(t.marked.is_empty(), "filtered-out rows are not selected");
    }
}
