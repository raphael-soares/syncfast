//! `syncfast-core` must stay free of UI crates (ADR 0001).

use std::collections::{HashMap, VecDeque};

use cargo_metadata::{DependencyKind, MetadataCommand, PackageId};

const ALLOWED_FASTFRAME: [&str; 4] = [
    "fastframe-file",
    "fastframe-instance",
    "fastframe-notifications",
    "fastframe-desktop",
];
const FORBIDDEN: [&str; 3] = ["egui", "eframe", "winit"];

fn is_forbidden(name: &str) -> bool {
    FORBIDDEN.contains(&name)
        || (name.starts_with("fastframe-") && !ALLOWED_FASTFRAME.contains(&name))
}

#[test]
fn core_normal_dependencies_exclude_ui_crates() {
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let metadata = MetadataCommand::new()
        .manifest_path(manifest)
        .exec()
        .expect("cargo metadata runs");
    let resolve = metadata
        .resolve
        .as_ref()
        .expect("metadata has a resolve graph");
    let root = metadata
        .packages
        .iter()
        .find(|p| p.name.as_str() == "syncfast-core")
        .expect("syncfast-core is in the workspace")
        .id
        .clone();
    let names: HashMap<&PackageId, &str> = metadata
        .packages
        .iter()
        .map(|p| (&p.id, p.name.as_str()))
        .collect();
    let nodes: HashMap<&PackageId, _> = resolve.nodes.iter().map(|n| (&n.id, n)).collect();

    let mut parent: HashMap<PackageId, Option<PackageId>> = HashMap::new();
    parent.insert(root.clone(), None);
    let mut queue = VecDeque::from([root.clone()]);
    let mut violations = Vec::new();
    while let Some(id) = queue.pop_front() {
        let Some(node) = nodes.get(&id) else { continue };
        for dep in &node.deps {
            let normal = dep
                .dep_kinds
                .iter()
                .any(|k| k.kind == DependencyKind::Normal);
            if !normal || parent.contains_key(&dep.pkg) {
                continue;
            }
            parent.insert(dep.pkg.clone(), Some(id.clone()));
            if is_forbidden(names[&dep.pkg]) {
                violations.push(dep.pkg.clone());
            }
            queue.push_back(dep.pkg.clone());
        }
    }

    let paths: Vec<String> = violations
        .iter()
        .map(|bad| {
            let mut chain = vec![names[bad]];
            let mut cur = bad;
            while let Some(Some(p)) = parent.get(cur) {
                chain.push(names[p]);
                cur = p;
            }
            chain.reverse();
            chain.join(" -> ")
        })
        .collect();
    assert!(
        paths.is_empty(),
        "syncfast-core pulls in UI crates:\n{}",
        paths.join("\n")
    );
}
