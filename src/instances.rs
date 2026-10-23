//! Weitere Bibliotheken des Add-ons, neben der aus „Konfiguration“: angelegt,
//! umbenannt und entfernt in der Web-UI (panel.rs), gemerkt in
//! /data/instances.json. Jede liegt in einem eigenen Ordner in share
//! (`<ordner>-<id>`), hat einen eigenen Token und ist unter
//! `https://…:4174/i/<id>` zu erreichen — für die App ein Server wie jeder
//! andere.
//!
//! Entfernen nimmt sie nur aus dem Dienst; der Ordner bleibt. Wer später eine
//! mit demselben Namen anlegt, bekommt ihn samt Songs zurück.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chordwright_server::{is_instance_id, Hub, Library};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// Unterordner in share.
    pub folder: String,
    pub token: String,
}

#[derive(Serialize, Deserialize, Default)]
struct File {
    instances: Vec<Entry>,
}

pub struct Registry {
    file: PathBuf,
    share: PathBuf,
    /// Der Ordner der Hauptbibliothek; die weiteren heißen `<base>-<id>`.
    base: String,
    backup_every: f64,
    backup_keep: usize,
    hub: Hub,
    list: Mutex<Vec<Entry>>,
}

/// Aus einem Namen eine id: klein, Umlaute ausgeschrieben, alles andere `-`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_lowercase().chars() {
        match c {
            'a'..='z' | '0'..='9' => out.push(c),
            'ä' => out.push_str("ae"),
            'ö' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            _ if !out.ends_with('-') && !out.is_empty() => out.push('-'),
            _ => {}
        }
    }
    let mut out: String = out.chars().take(32).collect();
    while out.ends_with('-') {
        out.pop();
    }
    out
}

pub fn new_token() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Ein Name, wie er in der Liste steht: eine Zeile, nicht zu lang.
fn clean_name(name: &str) -> Option<String> {
    let name: String = name
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(60)
        .collect();
    (!name.is_empty()).then_some(name)
}

impl Registry {
    /// Liest /data/instances.json und nimmt jede darin in den Dienst. Eine,
    /// die nicht startet, steht im Log und bleibt in der Liste.
    pub fn open(
        data: &Path,
        share: &Path,
        base: &str,
        backup_every: f64,
        backup_keep: usize,
        hub: Hub,
    ) -> Registry {
        let file = data.join("instances.json");
        let list = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str::<File>(&t).ok())
            .unwrap_or_default()
            .instances
            .into_iter()
            .filter(|e| is_instance_id(&e.id) && !e.token.is_empty())
            .collect::<Vec<_>>();
        let registry = Registry {
            file,
            share: share.to_path_buf(),
            base: base.to_string(),
            backup_every,
            backup_keep,
            hub,
            list: Mutex::new(Vec::new()),
        };
        for entry in &list {
            if let Err(e) = registry.serve(entry) {
                eprintln!("Bibliothek {}: {e}", entry.name);
            }
        }
        *registry.lock() = list;
        registry
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.list.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn serve(&self, entry: &Entry) -> Result<(), String> {
        self.hub.add(
            &entry.id,
            Library {
                dir: self.dir_of(entry),
                token: entry.token.clone(),
                backup_every: self.backup_every,
                backup_keep: self.backup_keep,
            },
        )
    }

    pub fn dir_of(&self, entry: &Entry) -> PathBuf {
        self.share.join(&entry.folder)
    }

    fn save(&self, list: &[Entry]) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&serde_json::json!({ "instances": list }))
            .map_err(|e| e.to_string())?;
        let tmp = self.file.with_extension("json.tmp");
        let fail = |e: std::io::Error| format!("{}: {e}", self.file.display());
        crate::addon::write_private(&tmp, &format!("{text}\n")).map_err(fail)?;
        std::fs::rename(&tmp, &self.file).map_err(fail)
    }

    pub fn list(&self) -> Vec<Entry> {
        self.lock().clone()
    }

    /// Legt eine an und nimmt sie gleich in den Dienst. Die id kommt aus dem
    /// Namen; gibt es sie schon, mit `-2`, `-3` dahinter.
    pub fn create(&self, name: &str) -> Result<Entry, String> {
        let name = clean_name(name).ok_or("Ohne Namen geht es nicht.")?;
        let mut list = self.lock();
        let stem = match slug(&name) {
            s if s.is_empty() => "bibliothek".to_string(),
            s => s,
        };
        let mut id = stem.clone();
        let mut n = 2;
        while list.iter().any(|e| e.id == id) {
            let suffix = format!("-{n}");
            id = format!("{}{suffix}", &stem[..stem.len().min(32 - suffix.len())]);
            n += 1;
        }
        let entry = Entry {
            folder: format!("{}-{id}", self.base),
            id,
            name,
            token: new_token()?,
        };
        self.serve(&entry)?;
        list.push(entry.clone());
        if let Err(e) = self.save(&list) {
            list.pop();
            self.hub.remove(&entry.id);
            return Err(e);
        }
        println!(
            "Bibliothek „{}“ angelegt: {} unter /i/{}",
            entry.name,
            self.dir_of(&entry).display(),
            entry.id
        );
        Ok(entry)
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<(), String> {
        let name = clean_name(name).ok_or("Ohne Namen geht es nicht.")?;
        let mut list = self.lock();
        let entry = list
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or("Diese Bibliothek gibt es nicht.")?;
        entry.name = name;
        self.save(&list)
    }

    /// Ein neuer Token: Geräte mit dem alten kommen nicht mehr hinein.
    pub fn new_token(&self, id: &str) -> Result<(), String> {
        let mut list = self.lock();
        let entry = list
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or("Diese Bibliothek gibt es nicht.")?;
        let old = entry.token.clone();
        entry.token = new_token()?;
        let entry = entry.clone();
        self.hub.remove(id);
        self.serve(&entry)?;
        if let Err(e) = self.save(&list) {
            // Zurück zum alten, der noch in der Datei steht.
            if let Some(e) = list.iter_mut().find(|e| e.id == id) {
                e.token = old;
                let back = e.clone();
                self.hub.remove(id);
                let _ = self.serve(&back);
            }
            return Err(e);
        }
        println!("Bibliothek „{}“: neuer Token", entry.name);
        Ok(())
    }

    /// Nimmt sie aus dem Dienst; der Ordner bleibt.
    pub fn remove(&self, id: &str) -> Result<Entry, String> {
        let mut list = self.lock();
        let at = list
            .iter()
            .position(|e| e.id == id)
            .ok_or("Diese Bibliothek gibt es nicht.")?;
        let entry = list.remove(at);
        if let Err(e) = self.save(&list) {
            list.insert(at, entry);
            return Err(e);
        }
        self.hub.remove(id);
        println!(
            "Bibliothek „{}“ entfernt; der Ordner {} bleibt.",
            entry.name,
            self.dir_of(&entry).display()
        );
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("  Die Großen Bläser! "), "die-grossen-blaeser");
        assert_eq!(slug("§§"), "");
        assert_eq!(slug(&"a".repeat(40)).len(), 32);
        assert!(is_instance_id(&slug("Chor 2026")));
    }
}
