//! User-defined project merges, for repositories that were renamed or moved to another folder
//! (for example ShotNotebook -> ShotGallery -> ShotCollector). A merge groups all sessions of one
//! project under another one; chains resolve to the newest project. Merges are stored in
//! `project-merges.json` in the app's config folder. By default they only change grouping in
//! this app; a merge can also move the sessions to the new project's folder (see
//! [`crate::session_move`]), which it records so that unmerging can move them back.

use crate::session_move::MovedSession;
use crate::{Entry, Project};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

pub const FILE_NAME: &str = "project-merges.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectRef {
    pub key: String,
    pub name: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Merge {
    pub from: ProjectRef,
    pub into: ProjectRef,
    /// Sessions moved to the folder of `into`, in the order they were moved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub moved: Vec<MovedSession>,
}

/// A project merged into another one, as shown in the merge dialog.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedProject {
    #[serde(flatten)]
    pub project: ProjectRef,
    /// Sessions moved to the folder of the project it was merged into.
    pub moved: usize,
    /// Sessions still in the project's own folder.
    pub not_moved: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Merges {
    #[serde(default)]
    pub merges: Vec<Merge>,
}

/// Only repository and folder groups can be merged; app and "No folder" groups cannot.
pub fn is_mergeable(key: &str) -> bool {
    key.starts_with("repo:") || key.starts_with("path:")
}

impl Merges {
    pub fn load(path: &Path) -> Merges {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .map_err(|err| format!("Unable to create {}: {err}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|err| err.to_string())?;
        fs::write(path, text).map_err(|err| format!("Unable to write {}: {err}", path.display()))
    }

    /// Follows merges starting at `key`; returns the final key and the last merge target.
    pub fn resolve<'a>(&'a self, key: &str) -> (String, Option<&'a ProjectRef>) {
        let mut current = key.to_string();
        let mut target = None;
        let mut seen = HashSet::new();
        while seen.insert(current.clone()) {
            match self.merges.iter().find(|merge| merge.from.key == current) {
                Some(merge) => {
                    target = Some(&merge.into);
                    current = merge.into.key.clone();
                }
                None => break,
            }
        }
        (current, target)
    }

    /// Merges project `from` into `into` (both current group keys), replacing an earlier merge
    /// of `from`. Fails when the merge would create a cycle.
    pub fn add(&mut self, from: ProjectRef, into: ProjectRef) -> Result<(), String> {
        if !is_mergeable(&from.key) || !is_mergeable(&into.key) {
            return Err("Only repository and folder groups can be merged".to_string());
        }
        if self.resolve(&into.key).0 == from.key {
            return Err(format!(
                "{} is already merged into {}",
                into.name, from.name
            ));
        }
        // Sessions moved by an earlier merge of `from` stay recorded, so they can be moved back.
        let moved = self
            .merges
            .iter()
            .position(|merge| merge.from.key == from.key)
            .map(|index| self.merges.remove(index).moved)
            .unwrap_or_default();
        self.merges.push(Merge { from, into, moved });
        Ok(())
    }

    pub fn find(&self, key: &str) -> Option<&Merge> {
        self.merges.iter().find(|merge| merge.from.key == key)
    }

    /// (git root, repository) of each moved Copilot CLI session before it was moved.
    pub fn moved_roots(&self) -> Vec<(String, String)> {
        self.merges
            .iter()
            .flat_map(|merge| &merge.moved)
            .filter_map(|moved| match moved {
                MovedSession::Cli { before, .. } => {
                    Some((before.git_root.clone()?, before.repository.clone()?))
                }
                MovedSession::VscodeChat { .. } => None,
            })
            .collect()
    }

    pub fn find_mut(&mut self, key: &str) -> Option<&mut Merge> {
        self.merges.iter_mut().find(|merge| merge.from.key == key)
    }

    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.merges.len();
        self.merges.retain(|merge| merge.from.key != key);
        self.merges.len() != before
    }
}

/// Sets each entry's project from its folder-based project, applying merges, and flags
/// projects whose folders no longer exist.
pub fn apply(entries: &mut [Entry], merges: &Merges) {
    let mut live: HashMap<&str, &Project> = HashMap::new();
    let mut own_sessions: HashMap<&str, usize> = HashMap::new();
    for entry in entries.iter() {
        live.entry(entry.base_project.key.as_str())
            .or_insert(&entry.base_project);
        *own_sessions
            .entry(entry.base_project.key.as_str())
            .or_default() += 1;
    }

    let mut resolved: HashMap<String, Project> = HashMap::new();
    let mut final_of = |key: &str| -> Project {
        if let Some(project) = resolved.get(key) {
            return project.clone();
        }
        let (final_key, target) = merges.resolve(key);
        let mut project = match (live.get(final_key.as_str()), target) {
            (Some(project), _) => (*project).clone(),
            (None, Some(target)) => Project {
                key: target.key.clone(),
                name: target.name.clone(),
                detail: target.detail.clone(),
                ..Project::default()
            },
            (None, None) => live.get(key).map(|p| (*p).clone()).unwrap_or_default(),
        };
        project.missing = false;
        project.merged = merges
            .merges
            .iter()
            .filter(|merge| merges.resolve(&merge.from.key).0 == project.key)
            .map(|merge| MergedProject {
                project: merge.from.clone(),
                moved: merge.moved.len(),
                not_moved: own_sessions
                    .get(merge.from.key.as_str())
                    .copied()
                    .unwrap_or(0),
            })
            .collect();
        project
            .merged
            .sort_by_key(|merged| merged.project.name.to_lowercase());
        resolved.insert(key.to_string(), project.clone());
        project
    };
    let finals: Vec<Project> = entries
        .iter()
        .map(|entry| final_of(&entry.base_project.key))
        .collect();

    // A project's folder is missing when none of its sessions' folders exist any more.
    // Sessions without a local absolute folder (e.g. remote WSL chats) count as present.
    let mut exists_cache: HashMap<&str, bool> = HashMap::new();
    let mut present: HashSet<&str> = HashSet::new();
    for (entry, project) in entries.iter().zip(&finals) {
        if present.contains(project.key.as_str()) {
            continue;
        }
        let folder = entry.git_root.as_deref().or(entry.summary.cwd.as_deref());
        let exists = match folder {
            Some(folder) if Path::new(folder).is_absolute() => *exists_cache
                .entry(folder)
                .or_insert_with(|| Path::new(folder).is_dir()),
            _ => true,
        };
        if exists {
            present.insert(project.key.as_str());
        }
    }
    let missing: HashSet<String> = finals
        .iter()
        .filter(|project| is_mergeable(&project.key) && !present.contains(project.key.as_str()))
        .map(|project| project.key.clone())
        .collect();

    for (entry, mut project) in entries.iter_mut().zip(finals) {
        project.missing = missing.contains(&project.key);
        entry.summary.project = project;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(key: &str, name: &str) -> ProjectRef {
        ProjectRef {
            key: key.to_string(),
            name: name.to_string(),
            detail: String::new(),
        }
    }

    #[test]
    fn resolves_chains_and_rejects_cycles() {
        let notebook = project("path:c:/repos/shotnotebook", "ShotNotebook");
        let gallery = project("repo:luiscantero/shotgallery", "ShotGallery");
        let collector = project("repo:luiscantero/shotcollector", "ShotCollector");
        let mut merges = Merges::default();
        merges.add(notebook.clone(), gallery.clone()).unwrap();
        merges.add(gallery.clone(), collector.clone()).unwrap();
        assert_eq!(merges.resolve(&notebook.key).0, collector.key);
        assert_eq!(merges.resolve(&collector.key).0, collector.key);

        assert!(merges.add(collector.clone(), notebook.clone()).is_err());
        assert!(merges
            .add(project("app:x", "X"), collector.clone())
            .is_err());
        assert!(merges
            .add(gallery.clone(), project("none", "No folder"))
            .is_err());

        assert!(merges.remove(&gallery.key));
        assert_eq!(merges.resolve(&notebook.key).0, gallery.key);
        assert!(!merges.remove(&gallery.key));
    }

    #[test]
    fn survives_cycles_in_saved_file() {
        let a = project("path:/a", "a");
        let b = project("path:/b", "b");
        let merges = Merges {
            merges: vec![
                Merge {
                    from: a.clone(),
                    into: b.clone(),
                    moved: Vec::new(),
                },
                Merge {
                    from: b.clone(),
                    into: a.clone(),
                    moved: Vec::new(),
                },
            ],
        };
        // Terminates instead of looping forever.
        merges.resolve(&a.key);
    }

    #[test]
    fn saves_and_loads_merges() {
        let dir = std::env::temp_dir().join(format!(
            "copilot-session-manager-merges-{}",
            std::process::id()
        ));
        let path = dir.join("nested").join(FILE_NAME);
        assert!(Merges::load(&path).merges.is_empty());
        let mut merges = Merges::default();
        merges
            .add(project("path:/old", "old"), project("repo:me/new", "new"))
            .unwrap();
        merges
            .find_mut("path:/old")
            .unwrap()
            .moved
            .push(MovedSession::VscodeChat {
                id: "x".to_string(),
                from_storage: "a".into(),
                into_storage: "b".into(),
            });
        merges.save(&path).unwrap();
        let loaded = Merges::load(&path);
        assert_eq!(loaded.resolve("path:/old").0, "repo:me/new");
        assert_eq!(loaded.find("path:/old").unwrap().moved.len(), 1);

        // Merging again keeps the record of moved sessions.
        let mut merges = loaded;
        merges
            .add(
                project("path:/old", "old"),
                project("repo:me/other", "other"),
            )
            .unwrap();
        assert_eq!(merges.find("path:/old").unwrap().moved.len(), 1);
        fs::write(&path, "not json").unwrap();
        assert!(Merges::load(&path).merges.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
