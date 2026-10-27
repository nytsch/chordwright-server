//! Weitere Bibliotheken des Add-ons, neben der aus „Konfiguration“: angelegt,
//! umbenannt und entfernt in der Web-UI (panel.rs), gemerkt in
//! /data/instances.json. Jede liegt in einem eigenen Ordner in share
//! (`<ordner>-<id>`), hat einen eigenen Token und ist unter
//! `https://…:4174/i/<id>` zu erreichen — für die App ein Server wie jeder
//! andere.
//!
//! Jede Bibliothek, auch die Hauptbibliothek, lässt sich außerdem für die
//! Band freigeben: ein zweiter Token, der liest und auf der Bühne mitspielt,
//! aber nichts schreibt (`Band` im Server), befristet oder bis auf Widerruf.
//! Er steht mit in /data/instances.json und gilt nach einem Neustart weiter.
//!
//! Entfernen nimmt sie nur aus dem Dienst; der Ordner bleibt. Wer später eine
//! mit demselben Namen anlegt, bekommt ihn samt Songs zurück.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chordwright_server::{is_instance_id, Band, Hub, Library};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// Unterordner in share.
    pub folder: String,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band: Option<Band>,
}

#[derive(Serialize, Deserialize, Default)]
struct File {
    instances: Vec<Entry>,
    /// Die Freigabe der Hauptbibliothek.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    band: Option<Band>,
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
    main_band: Mutex<Option<Band>>,
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
        let saved = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str::<File>(&t).ok())
            .unwrap_or_default();
        hub.set_band(None, saved.band.clone());
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
            main_band: Mutex::new(saved.band),
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
        self.hub.set_band(Some(&entry.id), entry.band.clone());
        Ok(())
    }

    pub fn dir_of(&self, entry: &Entry) -> PathBuf {
        self.share.join(&entry.folder)
    }

    fn save(&self, list: &[Entry]) -> Result<(), String> {
        let band = self
            .main_band
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let text = serde_json::to_string_pretty(&File {
            instances: list.to_vec(),
            band,
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
            band: None,
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

    /// Die Freigabe für die Band; `id` leer: die der Hauptbibliothek.
    pub fn band(&self, id: &str) -> Option<Band> {
        if id.is_empty() {
            return self
                .main_band
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
        }
        self.lock().iter().find(|e| e.id == id)?.band.clone()
    }

    /// Gibt sie der Band frei, für `days` Tage ab jetzt (0: bis auf
    /// Widerruf). Gibt es schon eine Freigabe, bleibt ihr Token und nur das
    /// Ende rückt — verbundene Geräte bleiben verbunden.
    pub fn share_band(&self, id: &str, days: u32, now: i64) -> Result<Band, String> {
        let until = (days > 0).then(|| now + i64::from(days) * DAY_MS);
        let mut list = self.lock();
        let current = if id.is_empty() {
            self.main_band
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        } else {
            list.iter()
                .find(|e| e.id == id)
                .ok_or("Diese Bibliothek gibt es nicht.")?
                .band
                .clone()
        };
        let band = Band {
            token: match current {
                Some(b) => b.token,
                None => new_token()?,
            },
            until,
        };
        self.put_band(&mut list, id, Some(band.clone()))?;
        Ok(band)
    }

    /// Nimmt die Freigabe zurück: der Token der Band gilt nicht mehr.
    pub fn end_band(&self, id: &str) -> Result<(), String> {
        let mut list = self.lock();
        self.put_band(&mut list, id, None)
    }

    fn put_band(&self, list: &mut [Entry], id: &str, band: Option<Band>) -> Result<(), String> {
        let old = if id.is_empty() {
            std::mem::replace(
                &mut *self.main_band.lock().unwrap_or_else(|e| e.into_inner()),
                band.clone(),
            )
        } else {
            let entry = list
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or("Diese Bibliothek gibt es nicht.")?;
            std::mem::replace(&mut entry.band, band.clone())
        };
        if let Err(e) = self.save(list) {
            // Zurück zu dem, was noch in der Datei steht.
            if id.is_empty() {
                *self.main_band.lock().unwrap_or_else(|e| e.into_inner()) = old;
            } else if let Some(entry) = list.iter_mut().find(|e| e.id == id) {
                entry.band = old;
            }
            return Err(e);
        }
        self.hub.set_band((!id.is_empty()).then_some(id), band);
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

    #[test]
    fn the_band_keeps_its_token_and_survives_a_restart() {
        use chordwright_server::{Options, Server};
        let root = std::env::temp_dir().join(format!("cw-registry-band-{}", std::process::id()));
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
        let server = start();
        let registry = Registry::open(&data, &share, "chordwright", 0.0, 3, server.hub());
        let chor = registry.create("Chor").unwrap();

        let main = registry.share_band("", 7, 1_000).unwrap();
        assert_eq!(main.until, Some(1_000 + 7 * DAY_MS));
        let longer = registry.share_band("", 30, 2_000).unwrap();
        assert_eq!(longer.token, main.token, "extending keeps the token");
        assert_eq!(server.hub().band(None), Some(longer.clone()));
        let chor_band = registry.share_band(&chor.id, 0, 2_000).unwrap();
        assert_eq!(chor_band.until, None);
        assert_ne!(chor_band.token, main.token);
        assert!(registry.share_band("nope", 1, 0).is_err());

        // A new library token leaves the band's alone.
        registry.new_token(&chor.id).unwrap();
        assert_eq!(server.hub().band(Some(&chor.id)), Some(chor_band.clone()));

        drop(registry);
        drop(server);
        let server = start();
        let registry = Registry::open(&data, &share, "chordwright", 0.0, 3, server.hub());
        assert_eq!(server.hub().band(None), Some(longer));
        assert_eq!(server.hub().band(Some(&chor.id)), Some(chor_band));

        registry.end_band("").unwrap();
        assert_eq!(registry.band(""), None);
        assert_eq!(server.hub().band(None), None);
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
    }
}
