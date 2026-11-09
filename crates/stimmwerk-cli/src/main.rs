use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use stimmwerk_core::{Project, cues, export, import, lint, llm_cfg, locate, music, persona_dirs, prose, render, shorten, speakers, sync, target, voice};

/// Stimmwerk — Stimmen-Studio. JSON auf stdout, Fortschritt auf stderr. Exit 0 ok, 1 Fehler (bei `sync check|render|reroll` auch: Cue zu lang/überlappend; `sync import` meldet das nur; `cue ls|add` bei Cue-Fehlern), 2 Aufruf.
#[derive(Parser)]
#[command(name = "stimmwerk", version)]
struct Cli {
    /// Fortschritt als JSON-Zeilen auf stderr (render/reroll/export), z. B. {"ev":"line","i":3,"n":7,"state":"done","tts_secs":2.9}
    #[arg(long, global = true)]
    events: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Neues Projekt `<name>.studio/` (Name → $STIMMWERK_HOME/projects, Pfad → dort)
    New {
        path: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, default_value = "de")]
        lang: String,
    },
    /// Projektübersicht: Sprecher, Zeilen, geschätzte Dauer
    Open { path: String },
    /// Projekte verwalten: auflisten, umbenennen, in den Papierkorb
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Prüfen: Syntax, Sprecher, Personas, Tags (Exit 1 bei Fehlern)
    Lint { path: String },
    /// Skript bearbeiten
    Script {
        #[command(subcommand)]
        cmd: ScriptCmd,
    },
    /// Zeilen mit omnivoice rendern (Cache in takes/<quality>/) und nach out/mix.wav mischen; Ctrl-C bricht sofort ab (laufende omnivoice-Zeile wird beendet, fertige Zeilen bleiben im Cache)
    Render {
        path: String,
        /// nur diese Zeile (1-basiert) → out/line.wav, kein Mix
        #[arg(long)]
        line: Option<usize>,
        /// Cache ignorieren: neu rendern
        #[arg(long)]
        fresh: bool,
        #[arg(long, value_enum, default_value = "final")]
        quality: Q,
    },
    /// Neuer Take für Zeile N (bzw. --undo: vorheriger), danach neu mischen
    Reroll {
        path: String,
        line: usize,
        #[arg(long)]
        undo: bool,
        #[arg(long, value_enum, default_value = "final")]
        quality: Q,
    },
    /// Exportieren (rendert fehlende Zeilen): wav|mp3 auf Loudness-Preset, parts = parts/NN-<wer>.wav + manifest.json + wav.
    /// KI-Hinweis: gesprochene Schlusszeile (erster Sprecher, settings.disclosure_text); ID3 COMM + TXXX AI_GENERATED=true immer, auch mit --no-disclosure
    Export {
        path: String,
        #[arg(value_enum)]
        kind: K,
        /// podcast (-16 LUFS/-1.5 dBTP) | hoerbuch (-19/-3) | video (-14/-1); Standard: settings.loudness
        #[arg(long)]
        loudness: Option<export::Loudness>,
        /// ohne gesprochenen KI-Hinweis (Metadaten bleiben)
        #[arg(long)]
        no_disclosure: bool,
        /// Zielordner (Standard: <proj>/out)
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "final")]
        quality: Q,
        /// Sync-Projekt: auch mit zu langen/überlappenden Cues exportieren (Überhang bleibt hörbar)
        #[arg(long)]
        allow_too_long: bool,
    },
    /// Untertitel-Sync: SRT/VTT-Cues sprechen und in ihre Zeitfenster einpassen (passt | atempo bis 1,12× | zu lang | Überlappung).
    /// `stimmwerk sync in.srt` prüft eine Datei ohne Projekt (Schätzung). Exit 1, wenn ein Cue zu lang ist oder überlappt (Bericht trotzdem)
    #[command(args_conflicts_with_subcommands = true)]
    Sync {
        #[command(subcommand)]
        cmd: Option<SyncCmd>,
        /// Untertiteldatei (.srt/.vtt oder beliebige Endung, Format wird erkannt): nur prüfen, ohne Projekt
        file: Option<PathBuf>,
    },
    /// Besetzung: Sprecher-ID → Persona
    Speaker {
        #[command(subcommand)]
        cmd: SpeakerCmd,
    },
    /// Stimmen-Bibliothek: Personas in $STIMMWERK_HOME/voices (entwerfen, behalten, klonen mit Einwilligung, widerrufen, löschen)
    Voice {
        #[command(subcommand)]
        cmd: VoiceCmd,
    },
    /// LLM-Backends ($STIMMWERK_HOME/settings.toml): lokal (gemma-serve, vMLX, LM Studio, Ollama) oder online (Anthropic,
    /// OpenAI, OpenRouter – nur mit Schlüssel im Schlüsselbund, `--allow-online` und Projekt-Schalter `--online-ok`)
    Llm {
        #[command(subcommand)]
        cmd: LlmCmd,
    },
    /// Musik mit acestep erzeugen (Instrumental, Cache takes/music/<key>.wav; gleiche Angaben = Treffer ohne acestep; GPU-Sperre, Ctrl-C bricht ab)
    Music {
        #[command(subcommand)]
        cmd: MusicCmd,
    },
    /// Musik-/Geräusch-Cues des Projekts ([[cues]] in project.toml): auf eine Zeile oder Sekunde legen, Ducking, Loop, WAV-Import
    Cue {
        #[command(subcommand)]
        cmd: CueCmd,
    },
    /// Prosa (Text/Markdown) → Rollenskript per LLM: script.txt + Sprecher (Persona leer, Stimm-Vorschlag im Ergebnis)
    Prose {
        path: String,
        file: PathBuf,
        /// Backend-Name statt Projekt-/Standard-Backend
        #[arg(long)]
        backend: Option<String>,
        /// vorhandenes Skript ersetzen (Besetzung bleibt)
        #[arg(long)]
        replace: bool,
        /// Online-Anbieter: bestätigt, dass der Text den Mac verlassen darf (keine Personendaten)
        #[arg(long)]
        online_ack: bool,
    },
}

#[derive(Subcommand)]
enum MusicCmd {
    /// Instrumental erzeugen: --prompt englisch ohne Zahlen, --dur 10–600 s; Ergebnis: key (für `cue add --gen`), Pfad, cached
    Gen {
        path: String,
        #[arg(long)]
        prompt: String,
        /// Sekunden (acestep-Minimum 10)
        #[arg(long)]
        dur: u32,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// 0 = frei
        #[arg(long, default_value_t = 0)]
        bpm: u32,
        /// z. B. "D major"
        #[arg(long)]
        key: Option<String>,
        /// acestep --dit (turbo | sft | base | xl-turbo | …)
        #[arg(long)]
        model: Option<String>,
        /// Cache ignorieren: neu erzeugen
        #[arg(long)]
        force: bool,
    },
    /// Erzeugte Musik im Cache (Key, Prompt, Dauer, benutzt von Cues)
    Ls { path: String },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum CueKind {
    Music,
    Ambience,
    Sfx,
}

impl From<CueKind> for cues::Kind {
    fn from(k: CueKind) -> Self {
        match k {
            CueKind::Music => cues::Kind::Music,
            CueKind::Ambience => cues::Kind::Ambience,
            CueKind::Sfx => cues::Kind::Sfx,
        }
    }
}

#[derive(Subcommand)]
enum CueCmd {
    /// Cue anlegen: Start --at-line N (1-basiert, Einwürfe zählen) oder --at-sec S; Quelle --file assets/… oder --gen KEY (aus `music gen`)
    Add {
        path: String,
        #[arg(long, conflicts_with = "at_sec")]
        at_line: Option<usize>,
        #[arg(long)]
        at_sec: Option<f64>,
        #[arg(long, conflicts_with = "gen_key")]
        file: Option<String>,
        #[arg(long = "gen")]
        gen_key: Option<String>,
        #[arg(long, value_enum, default_value = "music")]
        kind: CueKind,
        /// dB re Vollausschlag der Quelle, −60..+6 (negativ: `--gain-db -20` oder `--gain-db=-20`)
        #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
        gain_db: f32,
        #[arg(long, default_value_t = 0.0)]
        fade_in: f64,
        #[arg(long, default_value_t = 0.0)]
        fade_out: f64,
        /// nahtlos wiederholen bis --loop-until bzw. bis zur letzten Zeile
        #[arg(long = "loop")]
        loop_: bool,
        /// Cue endet am Anfang dieser Zeile (Zeilenzahl + 1 = Ende)
        #[arg(long)]
        loop_until: Option<usize>,
        /// unter Sprache absenken ([music].duck_db, Standard −12 dB)
        #[arg(long)]
        duck: bool,
        /// eigene Absenkung in dB (−60..0), setzt --duck
        #[arg(long, allow_negative_numbers = true)]
        duck_db: Option<f32>,
    },
    /// Cues mit Prüfung (Exit 1 bei Fehlern)
    Ls { path: String },
    /// Cue N (1-basiert) entfernen; Dateien bleiben
    Rm { path: String, n: usize },
    /// WAV nach assets/<kind>/ kopieren (mono/stereo WAV unverändert, sonst per ffmpeg); Herkunft/Lizenz in assets/SOURCES.md
    Import {
        path: String,
        file: PathBuf,
        /// Dateiname ohne .wav (Standard: Quellname)
        #[arg(long = "as")]
        name: Option<String>,
        #[arg(long, value_enum, default_value = "ambience")]
        kind: CueKind,
        /// Herkunft (URL)
        #[arg(long)]
        source: Option<String>,
        /// z. B. CC0
        #[arg(long)]
        license: Option<String>,
    },
}

fn cue_list(p: &Project) -> Value {
    let is = cues::lint(p);
    let errors = is.iter().filter(|i| i.level == "error").count();
    for i in &is {
        eprintln!("{} {}", i.level, i.msg);
    }
    let list: Vec<Value> = p.meta.cues.iter().enumerate().map(|(k, c)| json!({"n": k + 1, "at_line": c.at_line, "at_sec": c.at_sec, "source": c.label(), "kind": c.kind, "gain_db": c.gain_db, "fade_in": c.fade_in, "fade_out": c.fade_out, "loop": c.looping(), "loop_until_line": c.loop_until_line, "duck": c.duck, "duck_db": c.duck_db})).collect();
    let v = json!({"project": p.dir, "cues": list, "issues": is, "errors": errors, "warnings": is.len() - errors});
    if errors > 0 { json!({"ok": false, "error": format!("{errors} Cue-Fehler"), "result": v}) } else { v }
}

fn music_cmd(cmd: MusicCmd) -> Result<Value> {
    Ok(match cmd {
        MusicCmd::Gen { path, prompt, dur, seed, bpm, key, model, force } => {
            on_ctrl_c();
            let p = Project::load(&locate(&path))?;
            let g = cues::Gen { prompt, dur, seed, bpm, key, model };
            let b = music::generate(&p, &g, force, &CANCEL, &prog)?;
            eprintln!("{}: {} ({} s) – `stimmwerk cue add {} --gen {} --at-line N`", if b.cached { "im Cache" } else { "erzeugt" }, b.path.display(), b.secs, path, b.key);
            json!(b)
        }
        MusicCmd::Ls { path } => {
            let p = Project::load(&locate(&path))?;
            json!({"dir": music::dir(&p), "music": music::ls(&p)})
        }
    })
}

fn cue_cmd(cmd: CueCmd) -> Result<Value> {
    Ok(match cmd {
        CueCmd::Add { path, at_line, at_sec, file, gen_key, kind, gain_db, fade_in, fade_out, loop_, loop_until, duck, duck_db } => {
            let mut p = Project::load(&locate(&path))?;
            if at_line.is_none() && at_sec.is_none() {
                bail!("--at-line N oder --at-sec S nötig");
            }
            let mut c = cues::Cue::new(kind.into());
            (c.at_line, c.at_sec, c.gain_db, c.fade_in, c.fade_out, c.loop_, c.loop_until_line, c.duck_db) = (at_line, at_sec, gain_db, fade_in, fade_out, loop_, loop_until, duck_db);
            c.duck = duck || duck_db.is_some();
            match (file, gen_key) {
                (Some(f), None) => c.file = Some(f),
                (None, Some(k)) => c.gen_ = Some(music::manifest(&p).remove(&k).with_context(|| format!("Musik {k} nicht im Cache – `stimmwerk music ls {path}`"))?.gen_),
                _ => bail!("--file assets/… oder --gen KEY nötig (genau eins)"),
            }
            p.meta.cues.push(c);
            let n = p.meta.cues.len();
            let bad: Vec<String> = cues::lint(&p).into_iter().filter(|i| i.level == "error" && i.msg.starts_with(&format!("Cue {n}:"))).map(|i| i.msg).collect();
            if !bad.is_empty() {
                bail!("{} – nichts geändert", bad.join("; "));
            }
            p.save_meta()?;
            eprintln!("Cue {n} angelegt");
            cue_list(&p)
        }
        CueCmd::Ls { path } => cue_list(&Project::load(&locate(&path))?),
        CueCmd::Rm { path, n } => {
            let mut p = Project::load(&locate(&path))?;
            if n == 0 || n > p.meta.cues.len() {
                bail!("Cue {n} gibt es nicht (1..={})", p.meta.cues.len());
            }
            let c = p.meta.cues.remove(n - 1);
            p.save_meta()?;
            eprintln!("Cue {n} ({}) entfernt", c.label());
            cue_list(&p)
        }
        CueCmd::Import { path, file, name, kind, source, license } => {
            let p = Project::load(&locate(&path))?;
            let r = music::import(&p, &file, name.as_deref(), kind.into(), source.as_deref(), license.as_deref(), &export::ffmpeg())?;
            eprintln!("{} ({} s, {} Hz) – `stimmwerk cue add {} --file {} --at-line N`", r.file, r.secs, r.sr, path, r.file);
            json!(r)
        }
    })
}

#[derive(Subcommand)]
enum LlmCmd {
    /// Konfigurierte Backends (Schlüssel nur: gesetzt/nicht) und Vorlagen
    Ls,
    /// Backend anlegen/ändern: Vorlage (gemma|vmlx|lmstudio|ollama|anthropic|openai|openrouter) oder --url/--kind/--model
    Add {
        name: String,
        #[arg(long, value_enum)]
        kind: Option<KindArg>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// Online-Anbieter erlauben (Projekte brauchen zusätzlich `llm use … --online-ok`)
        #[arg(long, conflicts_with = "no_online")]
        allow_online: bool,
        #[arg(long)]
        no_online: bool,
        /// als Standard für Projekte ohne eigenes Backend
        #[arg(long)]
        default: bool,
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Backend entfernen (ein Schlüssel im Schlüsselbund wird mit gelöscht)
    Rm { name: String },
    /// Verbindung prüfen: GET /models (online nur mit --allow-online)
    Test { name: String },
    /// Projekt auf Backend setzen; --online-ok = Projekt-Schalter (Text darf zum Online-Anbieter)
    Use {
        path: String,
        name: String,
        #[arg(long, conflicts_with = "no_online")]
        online_ok: bool,
        #[arg(long)]
        no_online: bool,
    },
    /// API-Schlüssel in den Schlüsselbund: Eingabe ohne Echo oder per stdin (`pbpaste | stimmwerk llm key NAME`); nie in
    /// Dateien oder Argumenten; --clear löscht ihn
    Key {
        name: String,
        #[arg(long)]
        clear: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum KindArg {
    Openai,
    Anthropic,
}

fn llm_cmd(cmd: LlmCmd) -> Result<Value> {
    let mut st = llm_cfg::load()?;
    let entry = |n: &str, e: &llm_cfg::Entry, dflt: bool| json!({"name": n, "kind": e.kind, "url": e.url, "model": e.model, "local": e.is_local(), "allow_online": e.allow_online, "key": e.key_info(), "timeout_s": e.timeout_s, "default": dflt});
    Ok(match cmd {
        LlmCmd::Ls => {
            let a = &st.llm;
            let bs: Vec<Value> = a.backends.iter().map(|(n, e)| entry(n, e, a.default.as_deref() == Some(n))).collect();
            let ps: Vec<Value> = llm_cfg::PRESETS.iter().map(|p| json!({"name": p.name, "label": p.label, "kind": p.kind, "url": p.url, "model": p.model, "online": p.online(), "configured": a.backends.contains_key(p.name)})).collect();
            json!({"settings": llm_cfg::path(), "default": a.default, "backends": bs, "presets": ps})
        }
        LlmCmd::Add { name, kind, url, model, allow_online, no_online, default, timeout } => {
            if !llm_cfg::name_ok(&name) {
                bail!("Backend-Name {name:?} ungültig (Buchstaben, Ziffern, _ - .)");
            }
            let mut e = st.llm.backends.get(&name).cloned().or_else(|| llm_cfg::Preset::get(&name).map(|p| p.entry())).unwrap_or_else(|| llm_cfg::Entry { url: String::new(), model: String::new(), ..llm_cfg::Entry::default() });
            if let Some(k) = kind {
                e.kind = match k {
                    KindArg::Openai => funkhaus_llm::Kind::OpenAi,
                    KindArg::Anthropic => funkhaus_llm::Kind::Anthropic,
                };
            }
            if let Some(u) = url {
                e.url = u;
            }
            if let Some(m) = model {
                e.model = m;
            }
            if let Some(t) = timeout {
                e.timeout_s = t;
            }
            if e.url.is_empty() {
                bail!("--url fehlt ({name} ist keine Vorlage: {})", llm_cfg::PRESETS.iter().map(|p| p.name).collect::<Vec<_>>().join(", "));
            }
            if allow_online {
                e.allow_online = true;
            }
            if no_online {
                e.allow_online = false;
            }
            if e.allow_online && !e.is_local() {
                eprintln!("Hinweis: {name} ist ein Online-Anbieter ({}) – Texte freigegebener Projekte verlassen den Mac. Schlüssel: `stimmwerk llm key {name}`", e.backend().host());
            }
            st.llm.backends.insert(name.clone(), e.clone());
            if default || st.llm.default.is_none() {
                st.llm.default = Some(name.clone());
            }
            llm_cfg::save(&st)?;
            entry(&name, &e, st.llm.default.as_deref() == Some(&name))
        }
        LlmCmd::Rm { name } => {
            st.llm.get(&name)?;
            // keychain item only if the entry references one (local backends never touch `security`)
            llm_cfg::clear_key(&mut st, &name)?;
            st.llm.backends.remove(&name);
            if st.llm.default.as_deref() == Some(&name) {
                st.llm.default = st.llm.backends.keys().find(|k| st.llm.backends[*k].is_local()).cloned();
            }
            llm_cfg::save(&st)?;
            json!({"removed": name, "default": st.llm.default})
        }
        LlmCmd::Test { name } => {
            let e = st.llm.get(&name)?;
            let t = std::time::Instant::now();
            let models = funkhaus_llm::models(&e.backend(), e.allow_online)?;
            json!({"name": name, "url": e.url, "local": e.is_local(), "model": e.model, "model_available": e.model.is_empty() || models.contains(&e.model), "models": models, "secs": (t.elapsed().as_secs_f64() * 100.0).round() / 100.0})
        }
        LlmCmd::Use { path, name, online_ok, no_online } => {
            let e = st.llm.get(&name)?.clone();
            let mut p = Project::load(&locate(&path))?;
            let mut l = p.meta.llm.clone().unwrap_or_default();
            l.backend = Some(name.clone());
            if online_ok {
                l.online_ok = true;
            }
            if no_online {
                l.online_ok = false;
            }
            p.meta.llm = Some(l.clone());
            p.save_meta()?;
            let r = llm_cfg::resolve(&st.llm, p.meta.llm.as_ref())?;
            if let Some(w) = r.warning().filter(|_| r.online_ok) {
                eprintln!("Hinweis: {w}");
            }
            let gate = r.allowed().err().map(|e| e.to_string());
            if let Some(g) = &gate {
                eprintln!("Hinweis: {g}");
            }
            json!({"project": p.dir, "backend": name, "local": e.is_local(), "online_ok": l.online_ok, "allow_online": e.allow_online, "usable": gate.is_none(), "blocked": gate})
        }
        LlmCmd::Key { name, clear } => {
            st.llm.get(&name)?;
            if clear {
                llm_cfg::clear_key(&mut st, &name)?;
                llm_cfg::save(&st)?;
                return Ok(json!({"name": name, "key": Value::Null}));
            }
            use std::io::{BufRead, IsTerminal};
            let tty = std::io::stdin().is_terminal();
            let _echo = tty.then(|| {
                eprint!("API-Schlüssel für {name} (Eingabe unsichtbar, landet nur im Schlüsselbund; alternativ: pbpaste | stimmwerk llm key {name}): ");
                NoEcho::on()
            });
            let mut key = String::new();
            let read = std::io::stdin().lock().read_line(&mut key);
            drop(_echo);
            read?;
            let key = key.trim();
            if key.is_empty() {
                bail!("kein Schlüssel eingegeben");
            }
            llm_cfg::set_key(&mut st, &name, key)?;
            llm_cfg::save(&st)?;
            eprintln!("Schlüssel für {name} im Schlüsselbund gespeichert (Dienst {}, Konto {name})", funkhaus_llm::keychain::SERVICE);
            json!({"name": name, "key": "keychain"})
        }
    })
}

/// Terminal echo off while a key is typed; `stty echo` again on drop (also on error/early return).
struct NoEcho(bool);
impl NoEcho {
    fn on() -> Self {
        NoEcho(stty("-echo"))
    }
}
impl Drop for NoEcho {
    fn drop(&mut self) {
        if self.0 {
            stty("echo");
            eprintln!();
        }
    }
}
fn stty(arg: &str) -> bool {
    std::process::Command::new("/bin/stty").arg(arg).stdin(std::process::Stdio::inherit()).status().is_ok_and(|s| s.success())
}

/// Backend for a project command (`--backend` overrides the project's name, consent stays the project's); online use
/// prints the personal-data warning and needs `--online-ack`.
fn llm_for(p: &Project, backend: Option<String>, online_ack: bool) -> Result<llm_cfg::Resolved> {
    let st = llm_cfg::load()?;
    let mut l = p.meta.llm.clone().unwrap_or_default();
    if backend.is_some() {
        l.backend = backend;
    }
    let r = llm_cfg::resolve(&st.llm, Some(&l))?;
    r.allowed()?;
    if let Some(w) = r.warning() {
        eprintln!("Hinweis: {w}");
        if !online_ack {
            bail!("{w} – mit --online-ack bestätigen oder ein lokales Backend wählen (`stimmwerk llm ls`)");
        }
    }
    Ok(r)
}

#[derive(Subcommand)]
enum SyncCmd {
    /// SRT/VTT importieren: eine Skriptzeile je Cue, Zeiten in sync.json; VTT <v Name> → Sprecher; leerer Cue = Stille
    Import {
        path: String,
        file: PathBuf,
        /// Sprecher-ID für Cues ohne <v Name> (Standard: erster Sprecher, sonst ERZ)
        #[arg(long)]
        speaker: Option<String>,
        /// vorhandenes Skript ersetzen (Besetzung bleibt)
        #[arg(long)]
        replace: bool,
    },
    /// Passt der Text in die Zeitfenster? Schätzung (13 Zeichen/s), ohne TTS. Projekt oder .srt/.vtt-Datei
    Check { path: String },
    /// Cues rendern (Cache takes/<quality>/sync), messen, atempo, nach out/sync.wav legen; Bericht je Cue
    Render {
        path: String,
        #[arg(long, value_enum, default_value = "final")]
        quality: Q,
    },
    /// Neuer Take für Cue N (bzw. --undo: vorheriger), danach Sync neu rendern
    Reroll {
        path: String,
        cue: usize,
        #[arg(long)]
        undo: bool,
        #[arg(long, value_enum, default_value = "final")]
        quality: Q,
    },
    /// Kürzungsvorschlag per LLM für zu lange Cues (oder --cue N): nur Vorschlag mit Prüfung (Zahlen, Länge), nichts wird geändert
    Shorten {
        path: String,
        #[arg(long)]
        cue: Option<usize>,
        #[arg(long)]
        backend: Option<String>,
        /// Online-Anbieter: bestätigt, dass die Cue-Texte den Mac verlassen dürfen
        #[arg(long)]
        online_ack: bool,
    },
}

/// Sync report as JSON; too_long/overlap -> envelope ok:false (exit 1) with the full report.
fn sync_out(r: &sync::Report) -> Value {
    let s = &r.summary;
    for c in r.cues.iter().filter(|c| c.warning.is_some()) {
        eprintln!("Cue {}: {}", c.index, c.warning.as_deref().unwrap_or_default());
    }
    r.warnings.iter().for_each(|w| eprintln!("Hinweis: {w}"));
    if s.ok {
        return json!(r);
    }
    json!({"ok": false, "error": format!("{} von {} Cues zu lang, {} überlappend", s.too_long, s.total, s.overlap), "result": r})
}

fn is_subtitle(p: &std::path::Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("srt") || e.eq_ignore_ascii_case("vtt"))
}

fn check_file(f: &std::path::Path) -> Result<Value> {
    let src = std::fs::read_to_string(f).with_context(|| f.display().to_string())?;
    Ok(sync_out(&sync::check_file(&src, &f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())?))
}

fn sync_project(path: &str) -> Result<(Project, sync::Sync)> {
    let p = Project::load(&locate(path))?;
    let s = sync::load(&p)?.with_context(|| format!("{} ist kein Sync-Projekt (sync.json fehlt) – erst `stimmwerk sync import`", p.dir.display()))?;
    Ok((p, s))
}

fn sync_cmd(cmd: Option<SyncCmd>, file: Option<PathBuf>) -> Result<Value> {
    let usage = |m: &str| -> ! {
        use clap::CommandFactory;
        Cli::command().error(clap::error::ErrorKind::InvalidValue, m).exit()
    };
    Ok(match (cmd, file) {
        (None, Some(f)) if is_subtitle(&f) || f.is_file() => check_file(&f)?,
        (None, Some(f)) => usage(&format!("{:?}: erwartet import|check|render|reroll|shorten oder eine Untertiteldatei (.srt/.vtt)", f.display().to_string())),
        (None, None) => usage("erwartet import|check|render|reroll|shorten oder eine Untertiteldatei (.srt/.vtt)"),
        (Some(SyncCmd::Import { path, file, speaker, replace }), _) => {
            let mut p = Project::load(&locate(&path))?;
            let src = std::fs::read_to_string(&file).with_context(|| file.display().to_string())?;
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let r = sync::import(&mut p, &src, &name, speaker.as_deref(), replace)?;
            r.warnings.iter().for_each(|w| eprintln!("Hinweis: {w}"));
            eprintln!("{} Cues importiert ({})", r.cues, r.format);
            let s = sync::load(&p)?.context("sync.json fehlt nach dem Import")?;
            json!({"import": r, "check": sync::check_project(&p, &s, &persona_dirs()), "project": p.summary()})
        }
        (Some(SyncCmd::Check { path }), _) => {
            let f = PathBuf::from(&path);
            if is_subtitle(&f) && f.is_file() {
                check_file(&f)?
            } else {
                let (p, s) = sync_project(&path)?;
                sync_out(&sync::check_project(&p, &s, &persona_dirs()))
            }
        }
        (Some(SyncCmd::Render { path, quality }), _) => {
            on_ctrl_c();
            let (p, s) = sync_project(&path)?;
            sync_out(&sync::render(&p, &s, &persona_dirs(), quality.into(), &CANCEL, &prog)?)
        }
        (Some(SyncCmd::Reroll { path, cue, undo, quality }), _) => {
            on_ctrl_c();
            let (p, s) = sync_project(&path)?;
            let r = sync::reroll(&p, &s, &persona_dirs(), cue, undo, quality.into(), &CANCEL, &prog)?;
            let v = sync_out(&r.report);
            let mut out = json!(r);
            if v["ok"] == false {
                out = json!({"ok": false, "error": v["error"], "result": out});
            }
            out
        }
        (Some(SyncCmd::Shorten { path, cue, backend, online_ack }), _) => {
            let (p, s) = sync_project(&path)?;
            let r = llm_for(&p, backend, online_ack)?;
            let v = shorten::run(&p, &s, &r, cue, &persona_dirs())?;
            for x in &v {
                match &x.proposal {
                    Some(t) if x.accepted => eprintln!("Cue {}: {} ({} → {} Zeichen, Ziel {})", x.cue, t, x.chars, x.proposal_chars, x.target_chars),
                    Some(t) => eprintln!("Cue {}: {} – Hinweis: {}", x.cue, t, x.violations.join("; ")),
                    None => eprintln!("Cue {}: kein brauchbarer Vorschlag ({})", x.cue, x.violations.join("; ")),
                }
            }
            if v.is_empty() {
                eprintln!("kein Cue zu lang – nichts vorzuschlagen");
            }
            json!({"backend": r.name, "online": !r.is_local(), "cues": v.len(), "accepted": v.iter().filter(|x| x.accepted).count(), "suggestions": v})
        }
    })
}

#[derive(Subcommand)]
enum VoiceCmd {
    /// Personas (eigene + funkhaus) mit Art, Herkunft, Einwilligung; omnivoice-Standardstimmen
    Ls,
    /// Eingebaute Beschreibungen für `design` (nur dokumentiertes omnivoice-Vokabular)
    Presets,
    /// Hörprobe einer Persona (Entwurfsqualität, gecacht in cache/probe)
    Probe {
        name: String,
        #[arg(long, default_value = voice::SAMPLE)]
        text: String,
        #[arg(long, default_value = "de")]
        lang: String,
        /// zusätzlich als <DIR>/<name>.wav ablegen
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// n Kandidaten aus einer Beschreibung (je ein Seed) → voices/candidates/<batch>/ + candidates.json; keine Persona
    Design {
        /// z. B. "female, middle-aged, low pitch"
        #[arg(long, conflicts_with = "preset")]
        instruct: Option<String>,
        /// Nummer aus `voice presets` (1-basiert)
        #[arg(long)]
        preset: Option<usize>,
        #[arg(long, default_value_t = 3)]
        n: usize,
        /// eigene Seeds, Komma-getrennt (Anzahl = --n)
        #[arg(long, value_delimiter = ',')]
        seeds: Option<Vec<u64>>,
        /// Probetext (wird bei `keep` zum Stimm-Anker)
        #[arg(long, default_value = voice::SAMPLE)]
        text: String,
        #[arg(long, default_value = "de")]
        lang: String,
        #[arg(long, value_enum, default_value = "final")]
        quality: Q,
    },
    /// Kandidat <id> aus <batch> als Design-Persona speichern (Anker-WAV in voices/clips/, instruct + seed in provenance)
    Keep {
        batch: String,
        id: usize,
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
    },
    /// Stimme aus einer WAV-Aufnahme klonen – nur mit Einwilligung (--consent); Ausschnitt nach voices/clips/, sha256-gebunden
    Clone {
        wav: PathBuf,
        #[arg(long)]
        name: String,
        /// exakt der gesprochene Text des Ausschnitts
        #[arg(long)]
        ref_text: String,
        #[arg(long)]
        start: Option<f64>,
        #[arg(long)]
        end: Option<f64>,
        /// wer, wann, wie zugestimmt hat (Pflicht; ohne: Exit 1, nichts geschrieben)
        #[arg(long)]
        consent: Option<String>,
        /// Name der Person, deren Stimme es ist (Pflicht)
        #[arg(long)]
        who: Option<String>,
        /// Datum der Einwilligung JJJJ-MM-TT (Standard: heute)
        #[arg(long)]
        consent_date: Option<String>,
        #[arg(long, default_value = "de")]
        lang: String,
    },
    /// Einwilligung eines Klons widerrufen: wird nicht mehr gerendert, alle Aufnahmen (Clip, Takes, Proben, Exporte,
    /// omnivoice-Cache) werden gelöscht; in der Persona bleibt nur ein Vermerk (Name, Einwilligung, Widerrufsdatum,
    /// Transkript der Aufnahme – für spätere Bereinigungen; `--forget` löscht es)
    Revoke {
        name: String,
        /// JJJJ-MM-TT (Standard: heute)
        #[arg(long)]
        date: Option<String>,
        /// auch das Transkript aus dem Vermerk löschen (danach findet `revoke --project` Takes weiterer Projekte nicht mehr)
        #[arg(long)]
        forget: bool,
        /// zusätzliches Projekt außerhalb des Projektordners (mehrfach möglich)
        #[arg(long)]
        project: Vec<String>,
    },
    /// Entworfene Stimme löschen, mit allen Aufnahmen (Klone: `voice revoke`)
    Rm {
        name: String,
        /// zusätzliches Projekt außerhalb des Projektordners (mehrfach möglich)
        #[arg(long)]
        project: Vec<String>,
    },
}

fn places(projects: &[String]) -> voice::Places {
    let on_wait: fn(&std::path::Path) = |p| eprintln!("wartet auf {} … (dort wird gerade gerendert)", p.display());
    voice::Places { on_wait: Some(on_wait), ..voice::Places::new(&projects.iter().map(|p| stimmwerk_core::locate(p)).collect::<Vec<_>>()) }
}

fn voice_cmd(cmd: VoiceCmd) -> Result<Value> {
    let (vdir, dirs) = (voice::dir(), persona_dirs());
    Ok(match cmd {
        VoiceCmd::Ls => json!({"builtins": lint::builtins(), "personas": voice::ls(&dirs), "dir": vdir}),
        VoiceCmd::Presets => json!(voice::PRESETS.iter().enumerate().map(|(i, (l, s))| json!({"n": i + 1, "label": l, "instruct": s})).collect::<Vec<_>>()),
        VoiceCmd::Probe { name, text, lang, out } => {
            on_ctrl_c();
            json!(voice::probe(&dirs, &name, &text, &lang, out.as_deref(), &CANCEL, &prog)?)
        }
        VoiceCmd::Design { instruct, preset, n, seeds, text, lang, quality } => {
            let instruct = match (instruct, preset) {
                (Some(i), _) => i,
                (None, Some(k)) => voice::PRESETS.get(k.wrapping_sub(1)).with_context(|| format!("--preset 1..={}", voice::PRESETS.len()))?.1.to_string(),
                (None, None) => bail!("--instruct \"…\" oder --preset N nötig (`stimmwerk voice presets`)"),
            };
            on_ctrl_c();
            let b = voice::design(&vdir, &voice::Design { instruct, n, seeds, text, lang, quality: quality.into() }, &CANCEL, &prog)?;
            for w in &b.warnings {
                eprintln!("Hinweis: {w}");
            }
            json!(b)
        }
        VoiceCmd::Keep { batch, id, name, description } => json!(voice::keep(&vdir, &dirs, lint::builtins(), &batch, id, &name, description.as_deref())?),
        VoiceCmd::Clone { wav, name, ref_text, start, end, consent, who, consent_date, lang } => {
            let c = voice::CloneReq { wav, name, ref_text, start, end, consent: consent.unwrap_or_default(), who: who.unwrap_or_default(), date: consent_date, lang };
            let r = voice::clone(&vdir, &dirs, lint::builtins(), &c)?;
            for w in &r.warnings {
                eprintln!("Hinweis: {w}");
            }
            json!(r)
        }
        VoiceCmd::Revoke { name, date, forget, project } => {
            let r = voice::revoke(&dirs, &name, date.as_deref(), forget, &places(&project))?;
            r.skipped.iter().for_each(|s| eprintln!("Warnung: {s}"));
            if r.ref_text_kept {
                eprintln!("Hinweis: {}", voice::REF_TEXT_NOTE);
            }
            json!(r)
        }
        VoiceCmd::Rm { name, project } => {
            let r = voice::rm(&vdir, &name, &places(&project))?;
            r.skipped.iter().for_each(|s| eprintln!("Warnung: {s}"));
            if !r.removed {
                eprintln!("Stimme '{name}' bleibt bestehen, bis alles gelöscht werden konnte – Problem beheben und erneut ausführen");
            }
            json!(r)
        }
    })
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Q {
    Draft,
    Final,
}

impl From<Q> for render::Quality {
    fn from(q: Q) -> Self {
        match q {
            Q::Draft => render::Quality::Draft,
            Q::Final => render::Quality::Final,
        }
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum K {
    Wav,
    Mp3,
    Parts,
}

static CANCEL: AtomicBool = AtomicBool::new(false);

static EVENTS: AtomicBool = AtomicBool::new(false);

/// `--events`: every event as one JSON line; otherwise the human text (as before: TTS lines, "Mische", warnings).
fn prog(e: render::Event) {
    use render::{Event as E, State};
    if EVENTS.load(Ordering::Relaxed) {
        eprintln!("{}", serde_json::to_string(&e).unwrap_or_default());
        return;
    }
    match e {
        E::Msg { text } => eprintln!("{text}"),
        E::Mix => eprintln!("Mische"),
        E::Master => eprintln!("Exportiere"),
        E::Line { i, state: State::Failed, .. } => eprintln!("Zeile {i} fehlgeschlagen"),
        _ => {}
    }
}

/// First Ctrl-C: cancel (the running omnivoice child is stopped, finished lines stay cached); second: exit at once (`exit` skips Drop: release the GPU prio files first).
fn on_ctrl_c() {
    let _ = ctrlc::set_handler(|| {
        if CANCEL.swap(true, Ordering::SeqCst) {
            funkhaus_sys::gpu::release_all();
            std::process::exit(130);
        }
        eprintln!("Abbruch … laufende Zeile wird beendet, fertige Zeilen bleiben im Cache (nochmal Ctrl-C: hart beenden)");
    });
}

#[derive(Subcommand)]
enum SpeakerCmd {
    /// Sprecher setzen/anlegen: Persona (Datei in voices/ oder omnivoice-Builtin), optional Anzeigename
    Set {
        path: String,
        id: String,
        #[arg(long)]
        persona: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// Sprecher mit Persona-Art und Zeilenzahl
    Ls { path: String },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Alle Projekte in $STIMMWERK_HOME/projects (Name, Pfad, geändert, Zeilen, Sync, Größe)
    Ls,
    /// Umbenennen: Ordner `<neu>.studio` und Titel; Pfade im Take-Index und in gui.json folgen. Abgelehnt, solange ein Job läuft
    Mv { old: String, new: String },
    /// In den macOS-Papierkorb verschieben (wiederherstellbar; Stimmen bleiben). Ohne --yes: Exit 2, nichts passiert
    Trash {
        name: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ScriptCmd {
    /// Text/Markdown anhängen: ein Absatz = eine Zeile; Überschriften → `# SEG n | Titel`
    Import {
        path: String,
        file: PathBuf,
        /// Sprecher-ID für Zeilen ohne erkannten Namen (Standard: erster Sprecher, sonst ERZ)
        #[arg(long)]
        speaker: Option<String>,
        /// `Name: Text` erkennen, neue Sprecher in project.toml (Persona leer)
        #[arg(long)]
        detect: bool,
    },
}

fn run(cmd: Cmd) -> Result<Value> {
    Ok(match cmd {
        Cmd::New { path, title, lang } => {
            let dir = target(&path);
            let title = title.unwrap_or_else(|| dir.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
            let p = Project::create(&dir, &title, &lang)?;
            eprintln!("Projekt angelegt: {}", dir.display());
            p.summary()
        }
        Cmd::Open { path } => Project::load(&locate(&path))?.summary(),
        Cmd::Project { cmd: ProjectCmd::Ls } => json!(stimmwerk_core::project::list()),
        Cmd::Project { cmd: ProjectCmd::Mv { old, new } } => {
            let d = locate(&old);
            let n = stimmwerk_core::project::rename(&d, &new)?;
            eprintln!("{} → {}", d.display(), n.display());
            json!(stimmwerk_core::project::info(&n)?)
        }
        Cmd::Project { cmd: ProjectCmd::Trash { name, yes } } => {
            let d = locate(&name);
            Project::load(&d)?;
            if !yes {
                use clap::CommandFactory;
                Cli::command().error(clap::error::ErrorKind::MissingRequiredArgument, format!("{} würde in den Papierkorb wandern – bestätige mit `stimmwerk project trash {name:?} --yes`", d.display())).exit()
            }
            let r = stimmwerk_core::project::trash(&d)?;
            eprintln!("Im Papierkorb: {}", r.path.display());
            eprintln!("Hinweis: {}", stimmwerk_core::project::TRASH_NOTE);
            r.moved_in.iter().for_each(|p| eprintln!("Eigener Export mit in den Papierkorb (jetzt unter out/{}): {}", stimmwerk_core::project::EXTERN, p.display()));
            r.outside.iter().for_each(|p| eprintln!("Bleibt liegen (anderes Laufwerk), weiter im Stimmen-Index: {}", p.display()));
            json!(r)
        }
        Cmd::Lint { path } => {
            let r = lint::lint(&locate(&path), &persona_dirs(), lint::builtins())?;
            for i in &r.issues {
                eprintln!("{} {}{}", i.level, i.line.map(|n| format!("Zeile {n}: ")).unwrap_or_default(), i.msg);
            }
            if r.errors > 0 {
                return Ok(json!({"ok": false, "error": format!("{} Fehler, {} Warnungen", r.errors, r.warnings), "result": r}));
            }
            json!(r)
        }
        Cmd::Script { cmd: ScriptCmd::Import { path, file, speaker, detect } } => {
            let mut p = Project::load(&locate(&path))?;
            let src = std::fs::read_to_string(&file).with_context(|| file.display().to_string())?;
            let r = import::import(&mut p, &src, speaker.as_deref(), detect)?;
            if r.lines == 0 {
                bail!("keine Zeilen in {}", file.display());
            }
            p.save()?;
            eprintln!("{} Zeilen, {} Kapitel importiert", r.lines, r.chapters);
            json!({"import": r, "project": p.summary()})
        }
        Cmd::Speaker { cmd: SpeakerCmd::Set { path, id, persona, label } } => {
            let mut p = Project::load(&locate(&path))?;
            if let Some(w) = speakers::set(&mut p, &persona_dirs(), lint::builtins(), &id, &persona, label.as_deref())? {
                eprintln!("Warnung: {w}");
            }
            p.save_meta()?;
            eprintln!("Sprecher {id} → {persona}");
            speakers::ls(&p, &persona_dirs(), lint::builtins())
        }
        Cmd::Render { path, line, fresh, quality } => {
            on_ctrl_c();
            let p = Project::load(&locate(&path))?;
            if p.dir.join(stimmwerk_core::project::SYNC).is_file() {
                eprintln!("Hinweis: Sync-Projekt – `stimmwerk sync render` legt die Cues auf ihre Zeiten (render reiht die Zeilen nur aneinander)");
            }
            json!(render::render(&p, &persona_dirs(), &render::Opts { line, fresh, quality: quality.into() }, &CANCEL, &prog)?)
        }
        Cmd::Reroll { path, line, undo, quality } => {
            on_ctrl_c();
            let p = Project::load(&locate(&path))?;
            json!(render::reroll(&p, &persona_dirs(), line, undo, quality.into(), &CANCEL, &prog)?)
        }
        Cmd::Export { path, kind, loudness, no_disclosure, out, quality, allow_too_long } => {
            on_ctrl_c();
            let p = Project::load(&locate(&path))?;
            let kind = match kind {
                K::Wav => export::Kind::Wav,
                K::Mp3 => export::Kind::Mp3,
                K::Parts => export::Kind::Parts,
            };
            let o = export::Opts { kind, loudness, disclosure: no_disclosure.then_some(false), out, quality: quality.into(), allow_too_long };
            json!(export::export(&p, &persona_dirs(), &o, &CANCEL, &prog)?)
        }
        Cmd::Voice { cmd } => voice_cmd(cmd)?,
        Cmd::Sync { cmd, file } => sync_cmd(cmd, file)?,
        Cmd::Llm { cmd } => llm_cmd(cmd)?,
        Cmd::Music { cmd } => music_cmd(cmd)?,
        Cmd::Cue { cmd } => cue_cmd(cmd)?,
        Cmd::Prose { path, file, backend, replace, online_ack } => {
            let mut p = Project::load(&locate(&path))?;
            let text = std::fs::read_to_string(&file).with_context(|| file.display().to_string())?;
            let r = llm_for(&p, backend, online_ack)?;
            eprintln!("Rollenskript mit {} ({}) …", r.name, if r.is_local() { "lokal" } else { "online" });
            let rep = prose::run(&mut p, &r, &text, replace)?;
            eprintln!("{} Zeilen, {} neue Sprecher in {} s", rep.lines, rep.speakers_added.len(), rep.elapsed_s);
            for c in &rep.cast {
                eprintln!("  {}: {} – {} – `stimmwerk voice design --instruct \"{}\"`", c.id, c.name, c.description, c.suggested_voice_instruct);
            }
            if !rep.cast_merged.is_empty() {
                eprintln!("Hinweis: {} kommen im Text nicht vor – ihre Zeilen spricht der Erzähler", rep.cast_merged.join(", "));
            }
            json!({"prose": rep, "project": p.summary()})
        }
        Cmd::Speaker { cmd: SpeakerCmd::Ls { path } } => speakers::ls(&Project::load(&locate(&path))?, &persona_dirs(), lint::builtins()),
    })
}

fn main() {
    let cli = Cli::parse();
    EVENTS.store(cli.events, Ordering::Relaxed);
    stimmwerk_core::set_app();
    funkhaus_sys::fix_path(); // GUI/launchd start with a minimal PATH (omnivoice, ffmpeg)
    let res = std::panic::catch_unwind(|| run(cli.cmd)).unwrap_or_else(|p| {
        let m = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
        Err(anyhow::anyhow!("interner Fehler: {m}"))
    });
    match res {
        Ok(v) if v["ok"] == false => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            std::process::exit(1);
        }
        Ok(v) => println!("{}", serde_json::to_string_pretty(&json!({"ok": true, "result": v})).unwrap()),
        Err(e) => {
            let msg = format!("{e:#}");
            eprintln!("Fehler: {msg}");
            println!("{}", json!({"ok": false, "error": msg}));
            std::process::exit(1);
        }
    }
}
