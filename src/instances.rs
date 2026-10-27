//! Weitere Bibliotheken des Add-ons, neben der aus „Konfiguration“: angelegt,
//! umbenannt und entfernt in der Web-UI (panel.rs), gemerkt in
//! /data/instances.json. Jede liegt in einem eigenen Ordner in share
//! (`<ordner>-<id>`), hat einen eigenen Token und ist unter
//! `https://…:4174/i/<id>` zu erreichen — für die App ein Server wie jeder
//! andere.
//!
//! Jede Bibliothek, auch die Hauptbibliothek, hat außerdem ihre Musiker
//! (`Member` im Server): Name, Rollen und ein eigener Token, befristet oder
//! bis auf Widerruf. Er liest, spielt auf der Bühne mit und schreibt nur die
//! eigenen Antworten, wann er kann. Die Liste steht mit in
//! /data/instances.json und gilt nach einem Neustart weiter.
//!
//! Entfernen nimmt sie nur aus dem Dienst; der Ordner bleibt. Wer später eine
//! mit demselben Namen anlegt, bekommt ihn samt Songs zurück.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chordwright_server::{is_instance_id, Can, Hub, Library, Member, ROLES};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// Unterordner in share.
    pub folder: String,
    pub token: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<Member>,
}

#[derive(Serialize, Deserialize, Default)]
struct File {
    instances: Vec<Entry>,
    /// Die Musiker der Hauptbibliothek.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    members: Vec<Member>,
}

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

pub struct Registry {
    file: PathBuf,
    share: PathBuf,
    /// Der Ordner der Hauptbibliothek; die weiteren heißen `<base>-<id>`.
    base: String,
    backup_every: f64,
    backup_keep: usize,
    hub: Hub,
    list: Mutex<Vec<Entry>>,
    /// Immer nach `list` sperren, nie umgekehrt.
    main_members: Mutex<Vec<Member>>,
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

fn until(days: u32, now: i64) -> Option<i64> {
    (days > 0).then(|| now + i64::from(days) * DAY_MS)
}

/// Nur Buchstaben aus `ROLES`, jeder einmal, in deren Reihenfolge.
fn clean_roles(roles: &[String]) -> Vec<String> {
    ROLES
        .iter()
        .map(|(l, _)| l.to_string())
        .filter(|l| roles.contains(l))
        .collect()
}

fn find_member<'a>(members: &'a mut [Member], id: &str) -> Result<&'a mut Member, String> {
    members
        .iter_mut()
        .find(|m| m.id == id)
        .ok_or_else(|| "Diesen Musiker gibt es nicht.".to_string())
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
        let saved = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str::<File>(&t).ok())
            .unwrap_or_default();
        hub.set_members(None, saved.members.clone());
        let list = saved
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
            main_members: Mutex::new(saved.members),
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
        )?;
        self.hub.set_members(Some(&entry.id), entry.members.clone());
        Ok(())
    }

    pub fn dir_of(&self, entry: &Entry) -> PathBuf {
        self.share.join(&entry.folder)
    }

    fn save(&self, list: &[Entry]) -> Result<(), String> {
        let members = self.main_members().clone();
        let text = serde_json::to_string_pretty(&File {
            instances: list.to_vec(),
            members,
        })
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
            members: Vec::new(),
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

    fn main_members(&self) -> std::sync::MutexGuard<'_, Vec<Member>> {
        self.main_members.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Die Musiker einer Bibliothek; `lib` leer: die der Hauptbibliothek.
    pub fn members(&self, lib: &str) -> Vec<Member> {
        if lib.is_empty() {
            return self.main_members().clone();
        }
        let list = self.lock();
        list.iter()
            .find(|e| e.id == lib)
            .map(|e| e.members.clone())
            .unwrap_or_default()
    }

    /// Nimmt einen Musiker auf, mit eigenem Token, für `days` Tage ab jetzt
    /// (0: bis auf Widerruf). Die id kommt aus dem Namen.
    pub fn add_member(
        &self,
        lib: &str,
        name: &str,
        roles: &[String],
        can: Can,
        days: u32,
        now: i64,
    ) -> Result<Member, String> {
        let name = clean_name(name).ok_or("Ohne Namen geht es nicht.")?;
        let token = new_token()?;
        let roles = clean_roles(roles);
        self.change_members(lib, |members| {
            let stem = match slug(&name) {
                s if s.is_empty() => "musiker".to_string(),
                s => s,
            };
            let mut id = stem.clone();
            let mut n = 2;
            while members.iter().any(|m| m.id == id) {
                let suffix = format!("-{n}");
                id = format!("{}{suffix}", &stem[..stem.len().min(32 - suffix.len())]);
                n += 1;
            }
            let member = Member {
                id,
                name,
                token,
                roles,
                until: until(days, now),
                can,
            };
            members.push(member.clone());
            Ok(member)
        })
    }

    /// Name und Rollen; Token und Frist bleiben.
    pub fn edit_member(
        &self,
        lib: &str,
        id: &str,
        name: &str,
        roles: &[String],
        can: Can,
    ) -> Result<(), String> {
        let name = clean_name(name).ok_or("Ohne Namen geht es nicht.")?;
        let roles = clean_roles(roles);
        self.change_members(lib, |members| {
            let m = find_member(members, id)?;
            m.name = name;
            m.roles = roles;
            m.can = can;
            Ok(())
        })
    }

    /// Ein neues Ende, ab jetzt; der Token bleibt, verbundene Geräte bleiben verbunden.
    pub fn extend_member(&self, lib: &str, id: &str, days: u32, now: i64) -> Result<(), String> {
        self.change_members(lib, |members| {
            find_member(members, id)?.until = until(days, now);
            Ok(())
        })
    }

    /// Nimmt ihn von der Liste: sein Token gilt nicht mehr. Seine Antworten
    /// bleiben im Ordner (`user/availability.<id>.json`).
    pub fn remove_member(&self, lib: &str, id: &str) -> Result<(), String> {
        self.change_members(lib, |members| {
            let at = members
                .iter()
                .position(|m| m.id == id)
                .ok_or("Diesen Musiker gibt es nicht.")?;
            members.remove(at);
            Ok(())
        })
    }

    /// Ändert die Musiker einer Bibliothek, schreibt die Datei und gibt sie
    /// dem Server; scheitert das Schreiben, bleibt alles, wie es war.
    fn change_members<T>(
        &self,
        lib: &str,
        change: impl FnOnce(&mut Vec<Member>) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut list = self.lock();
        let old = if lib.is_empty() {
            self.main_members().clone()
        } else {
            list.iter()
                .find(|e| e.id == lib)
                .ok_or("Diese Bibliothek gibt es nicht.")?
                .members
                .clone()
        };
        let mut members = old.clone();
        let out = change(&mut members)?;
        let put = |list: &mut Vec<Entry>, members: Vec<Member>| {
            if lib.is_empty() {
                *self.main_members() = members;
            } else if let Some(e) = list.iter_mut().find(|e| e.id == lib) {
                e.members = members;
            }
        };
        put(&mut list, members.clone());
        if let Err(e) = self.save(&list) {
            put(&mut list, old);
            return Err(e);
        }
        self.hub
            .set_members((!lib.is_empty()).then_some(lib), members);
        Ok(out)
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

    #[test]
    fn musicians_keep_their_tokens_and_survive_a_restart() {
        use chordwright_server::{Options, Server};
        let root = std::env::temp_dir().join(format!("cw-registry-members-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (data, share) = (root.join("data"), root.join("share"));
        std::fs::create_dir_all(&data).unwrap();
        let start = || {
            Server::start(Options {
                dir: share.join("chordwright"),
                token: "owner".into(),
                port: 0,
                ..Options::default()
            })
            .unwrap()
        };
        let roles = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let server = start();
        let registry = Registry::open(&data, &share, "chordwright", 0.0, 3, server.hub());
        let chor = registry.create("Chor").unwrap();

        let niko = registry
            .add_member(
                "",
                "Niko",
                &roles(&["S", "A", "X", "A"]),
                Can::Read,
                7,
                1_000,
            )
            .unwrap();
        assert_eq!(niko.id, "niko");
        assert_eq!(niko.roles, ["A", "S"], "known letters, once, in order");
        assert_eq!(niko.until, Some(1_000 + 7 * DAY_MS));
        let twin = registry
            .add_member("", "Niko", &[], Can::Read, 0, 1_000)
            .unwrap();
        assert_eq!(twin.id, "niko-2");
        assert_ne!(twin.token, niko.token);
        assert!(registry
            .add_member("nope", "Lea", &[], Can::Read, 1, 0)
            .is_err());
        assert!(registry.add_member("", "  ", &[], Can::Read, 1, 0).is_err());

        registry.extend_member("", "niko", 30, 2_000).unwrap();
        registry
            .edit_member("", "niko", "Niko K.", &roles(&["E"]), Can::Sets)
            .unwrap();
        let now = registry.members("");
        assert_eq!(now[0].token, niko.token, "extending keeps the token");
        assert_eq!(now[0].until, Some(2_000 + 30 * DAY_MS));
        assert_eq!(
            (now[0].name.as_str(), now[0].roles.clone()),
            ("Niko K.", roles(&["E"]))
        );
        assert_eq!(server.hub().members(None), Some(now.clone()));

        let lea = registry
            .add_member(&chor.id, "Lea", &roles(&["P"]), Can::All, 0, 0)
            .unwrap();
        // A new library token leaves the musicians alone.
        registry.new_token(&chor.id).unwrap();
        assert_eq!(
            server.hub().members(Some(&chor.id)),
            Some(vec![lea.clone()])
        );

        drop(registry);
        drop(server);
        let server = start();
        let registry = Registry::open(&data, &share, "chordwright", 0.0, 3, server.hub());
        assert_eq!(server.hub().members(None), Some(now));
        assert_eq!(server.hub().members(Some(&chor.id)), Some(vec![lea]));

        registry.remove_member("", "niko").unwrap();
        assert!(registry.remove_member("", "niko").is_err());
        assert_eq!(registry.members("").len(), 1);
        assert_eq!(server.hub().members(None).unwrap()[0].id, "niko-2");
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
    }
}
