use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use funkhaus_core::{Backend, Store, ask, doctor, make, podcast, util};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Funkhaus — lokales NotebookLM. JSON auf stdout, Fortschritt auf stderr. Exit 0 ok, 1 Fehler, 2 Aufruf.
#[derive(Parser)]
#[command(name = "funkhaus", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Notebooks verwalten
    Nb {
        #[command(subcommand)]
        cmd: NbCmd,
    },
    /// Quellen verwalten
    Src {
        #[command(subcommand)]
        cmd: SrcCmd,
    },
    /// Frage an die Quellen (Antwort mit Zitaten)
    Ask { nb: String, question: String },
    /// Drei Fragevorschläge zu den Quellen
    Suggest { nb: String },
    /// Chatverlauf zeigen (--clear löscht ihn)
    Chat {
        nb: String,
        #[arg(long)]
        clear: bool,
    },
    /// Chunk-Text zu einer Zitat-ID (z. B. s1#3)
    Chunk { nb: String, id: String },
    /// Notizen
    Note {
        #[command(subcommand)]
        cmd: NoteCmd,
    },
    /// Dokument erzeugen (Markdown)
    Make {
        nb: String,
        #[arg(value_parser = ["summary", "faq", "briefing", "studyguide", "timeline"])]
        kind: String,
    },
    /// Podcast erzeugen: Skript → omnivoice → Mix → MP3
    Podcast {
        nb: String,
        #[arg(long)]
        focus: Option<String>,
        #[arg(long, default_value = "mittel", value_parser = ["kurz", "mittel", "lang"])]
        len: String,
        /// O,F z. B. gt,fable
        #[arg(long)]
        voices: Option<String>,
        #[arg(long, default_value = "de")]
        lang: String,
        #[arg(long)]
        script_only: bool,
        #[arg(long)]
        from_script: Option<PathBuf>,
        /// Lautheits-Preset (sonst config.toml)
        #[arg(long, value_parser = ["podcast", "hoerbuch", "video"])]
        loudness: Option<String>,
        /// Gesprochenen KI-Schlusssatz weglassen
        #[arg(long)]
        no_disclosure: bool,
        /// TTS-Cache ignorieren: alle Zeilen neu rendern
        #[arg(long)]
        fresh: bool,
        /// Intro/Outro erzwingen (Fehler, wenn Dateien fehlen)
        #[arg(long, conflicts_with = "no_jingle")]
        jingle: bool,
        #[arg(long)]
        no_jingle: bool,
        /// Musikbett mit Ducking unter der Sprache
        #[arg(long)]
        bed: bool,
    },
    /// Jingle + Musikbett (acestep, einmal erzeugt)
    Music {
        #[command(subcommand)]
        cmd: MusicCmd,
    },
    /// Eine Zeile (1-basiert, Dialogzeilen von script.txt) neu sprechen lassen und neu mischen
    Reroll {
        nb: String,
        line: usize,
        /// Podcast-Ordner (Name oder Pfad) oder latest
        #[arg(long, default_value = "latest")]
        dir: String,
        /// Zum vorigen Take zurück, ohne neu zu rendern
        #[arg(long)]
        undo: bool,
    },
    /// Bisherige Podcasts eines Notebooks
    Podcasts { nb: String },
    /// Quellen kartieren: pro Chunk Kontext, Zusammenfassung, Fakten mit Zitat, Fragen (LLM, zwischengespeichert)
    Map {
        nb: String,
        #[arg(long)]
        source: Option<String>,
        /// kleines Modell (vMLX :8080, Gemma E2B)
        #[arg(long)]
        fast: bool,
    },
    /// Quellenführer: Abschnitte (Reduce aus Fakten+Zitaten) und Überblick/Themen/Fragen pro Quelle (LLM, zwischengespeichert)
    Guide {
        nb: String,
        #[arg(long)]
        source: Option<String>,
    },
    /// Quellenführer prüfen (R1–R5, G1–G6, kein LLM): JSONL {stage, check, pass, detail} auf stdout
    EvalGuide { nb: String },
    /// 3-Stufen-Podcast prüfen (O1–O9, S1–S11, kein LLM): JSONL {stage, check, pass, detail} auf stdout
    EvalPodcast { nb: String, dir: PathBuf },
    /// Retrieval-Evaluation gegen Gold-Zitate (kein LLM)
    Eval {
        nb: String,
        #[arg(long)]
        set: PathBuf,
        /// z. B. 1,3,5
        #[arg(long, default_value = "1,3,5")]
        k: String,
        /// bm25 | fields | fields:W1,W2,W3
        #[arg(long, default_value = "fields")]
        ranker: String,
    },
    /// Aussprache-Lexikon des Notebooks (notebooks/<id>/lexicon.tsv)
    Lex {
        nb: String,
        #[command(subcommand)]
        cmd: LexCmd,
    },
    /// Stimmen (Personas in <FUNKHAUS_HOME>/voices/)
    Voice {
        #[command(subcommand)]
        cmd: VoiceCmd,
    },
    /// LLM-Endpoint, omnivoice, ffmpeg prüfen
    Doctor,
}

#[derive(Subcommand)]
enum LexCmd {
    Ls,
    /// Zeile hinzufügen oder ersetzen
    Add {
        key: String,
        speakable: String,
        #[arg(long, default_value = "unverified")]
        note: String,
    },
    Rm { key: String },
}

#[derive(Subcommand)]
enum VoiceCmd {
    Ls,
    Show { name: String },
    /// Persona anlegen (clone nur mit Einwilligung; --ref nur bei clone)
    Add {
        name: String,
        #[arg(long, value_parser = ["builtin", "design", "clone"])]
        kind: String,
        #[arg(long)]
        voice: Option<String>,
        #[arg(long)]
        instruct: Option<String>,
        #[arg(long = "ref")]
        ref_: Option<PathBuf>,
        #[arg(long)]
        ref_text: Option<String>,
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        seed: Option<u64>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        consent_who: Option<String>,
        #[arg(long)]
        consent_statement: Option<String>,
        /// JJJJ-MM-TT, Standard heute
        #[arg(long)]
        consent_date: Option<String>,
        #[arg(long)]
        consent_recording: Option<String>,
    },
    /// Einen Satz mit omnivoice rendern (GPU)
    Test {
        name: String,
        text: String,
    },
}

#[derive(Subcommand)]
enum NbCmd {
    New { name: String },
    List,
    Show { nb: String },
    Rm { nb: String },
    /// LLM-Backend; "claude" schickt Quelltexte an Anthropic (nur mit Erlaubnis!)
    Backend {
        nb: String,
        #[arg(value_parser = ["local", "claude"])]
        backend: String,
    },
}

#[derive(Subcommand)]
enum SrcCmd {
    /// Datei, Ordner oder URL hinzufügen
    Add {
        nb: String,
        #[arg(required = true)]
        inputs: Vec<String>,
    },
    List { nb: String },
    Rm { nb: String, src: String },
    Toggle {
        nb: String,
        src: String,
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Extrahierten Text einer Quelle zeigen
    Show { nb: String, src: String },
    /// Kontextstufe einer Quelle: aus (nur Rohtext), kurz (Kurzfassung), voll
    Ctx {
        nb: String,
        src: String,
        #[arg(value_parser = ["aus", "kurz", "voll"])]
        level: String,
    },
    /// Zeitbereiche ausblenden (nie indexiert, zitiert oder vertont): --add 46:18-48:35 | --list | --rm N
    Outcut {
        nb: String,
        src: String,
        /// "t0-t1", Zeiten als h:mm:ss, mm:ss oder Sekunden
        #[arg(long)]
        add: Option<String>,
        #[arg(long, default_value = "manuell")]
        reason: String,
        #[arg(long)]
        list: bool,
        /// Nummer aus --list (ab 1)
        #[arg(long)]
        rm: Option<usize>,
    },
}

#[derive(Subcommand)]
enum NoteCmd {
    /// Notiz anlegen; ohne TEXT mit --last = letzte Chat-Antwort
    Add {
        nb: String,
        text: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        last: bool,
    },
    List { nb: String },
    Rm { nb: String, id: String },
}

#[derive(Subcommand)]
enum MusicCmd {
    /// Intro/Outro/Bett nach <FUNKHAUS_HOME>/music/ rendern (GPU-Lock; unverändertes wird übersprungen)
    Make {
        #[arg(long)]
        seed: Option<u64>,
        /// ersetzt alle drei Captions
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// Dateien, Dauer, Cache-Keys (JSON)
    Show,
}

fn prog(s: &str) {
    eprintln!("{s}");
}

fn run(cmd: Cmd) -> Result<Value> {
    let st = Store::open()?;
    Ok(match cmd {
        Cmd::Nb { cmd } => match cmd {
            NbCmd::New { name } => json!(st.create(&name)?),
            NbCmd::List => json!(st.list()?.iter().map(|n| json!({"id": n.id, "name": n.name, "created": n.created, "sources": n.sources.len(), "backend": n.backend})).collect::<Vec<_>>()),
            NbCmd::Show { nb } => {
                let n = st.get(&nb)?;
                json!({"notebook": n, "dir": st.dir(&n.id), "notes": st.notes(&n)?.len(), "podcasts": podcast::list(&st, &n)})
            }
            NbCmd::Rm { nb } => {
                let n = st.get(&nb)?;
                st.remove(&n)?;
                json!({"removed": n.id})
            }
            NbCmd::Backend { nb, backend } => {
                let mut n = st.get(&nb)?;
                st.set_backend(&mut n, if backend == "claude" { Backend::Claude } else { Backend::Local })?;
                json!({"id": n.id, "backend": n.backend})
            }
        },
        Cmd::Src { cmd } => match cmd {
            SrcCmd::Add { nb, inputs } => {
                let mut n = st.get(&nb)?;
                let mut added = vec![];
                for i in &inputs {
                    added.extend(st.add_source(&mut n, i, &prog)?);
                }
                json!({"added": added, "total": n.sources.len()})
            }
            SrcCmd::List { nb } => json!(st.get(&nb)?.sources),
            SrcCmd::Rm { nb, src } => json!({"removed": st.rm_source(&mut st.get(&nb)?, &src)?}),
            SrcCmd::Toggle { nb, src, state } => json!(st.toggle(&mut st.get(&nb)?, &src, state == "on")?),
            SrcCmd::Ctx { nb, src, level } => json!(st.set_ctx(&mut st.get(&nb)?, &src, funkhaus_core::store::Ctx::parse(&level)?)?),
            SrcCmd::Outcut { nb, src, add, reason, list: _, rm } => {
                use funkhaus_core::store::OutcutOp;
                let mut n = st.get(&nb)?;
                let op = match (add, rm) {
                    (Some(a), None) => {
                        let (x, y) = a.split_once('-').context("--add t0-t1")?;
                        let t = |s: &str| funkhaus_core::ingest::ts(s).with_context(|| format!("Zeit: {s}"));
                        OutcutOp::Add(t(x)?, t(y)?, reason)
                    }
                    (None, Some(i)) => OutcutOp::Rm(i),
                    (None, None) => OutcutOp::List,
                    _ => bail!("--add oder --rm, nicht beides"),
                };
                let v = st.outcut(&mut n, &src, op)?;
                json!(v.iter().enumerate().map(|(i, o)| json!({"n": i + 1, "t0": o.t0, "t1": o.t1, "from": funkhaus_sys::mmss(o.t0), "to": funkhaus_sys::mmss(o.t1), "reason": o.reason, "src": o.src})).collect::<Vec<_>>())
            }
            SrcCmd::Show { nb, src } => {
                let mut n = st.get(&nb)?;
                let s = Store::source_mut(&mut n, &src)?.clone();
                json!({"source": s, "text": st.text(&n, &s.id)?})
            }
        },
        Cmd::Ask { nb, question } => json!(ask::ask(&st, &st.get(&nb)?, &question, &prog)?),
        Cmd::Suggest { nb } => json!(ask::suggest(&st, &st.get(&nb)?)?),
        Cmd::Chat { nb, clear } => {
            let n = st.get(&nb)?;
            if clear {
                st.clear_chat(&n)?;
                json!({"cleared": n.id})
            } else {
                json!(st.chat::<ask::Answer>(&n)?)
            }
        }
        Cmd::Chunk { nb, id } => json!(st.chunk(&st.get(&nb)?, &id)?),
        Cmd::Note { cmd } => match cmd {
            NoteCmd::Add { nb, text, title, last } => {
                let n = st.get(&nb)?;
                let (t, body) = match (text, last) {
                    (Some(t), _) => (title.unwrap_or_else(|| funkhaus_sys::clip(&t, 50)), t),
                    (None, true) => {
                        let a = st.chat::<ask::Answer>(&n)?.pop().context("kein Chatverlauf")?;
                        let mut body = a.text.clone();
                        for c in &a.cites {
                            body.push_str(&format!("\n\n[{}] {} ({}): {}", c.n, c.title, c.loc(), c.excerpt));
                        }
                        (title.unwrap_or(a.q), body)
                    }
                    (None, false) => bail!("TEXT oder --last angeben"),
                };
                json!(st.add_note(&n, &t, &body)?)
            }
            NoteCmd::List { nb } => json!(st.notes(&st.get(&nb)?)?),
            NoteCmd::Rm { nb, id } => {
                st.rm_note(&st.get(&nb)?, &id)?;
                json!({"removed": id})
            }
        },
        Cmd::Make { nb, kind } => json!(make::make(&st, &st.get(&nb)?, &kind, &prog)?),
        Cmd::Podcast { nb, focus, len, voices, lang, script_only, from_script, loudness, no_disclosure, fresh, jingle, no_jingle, bed } => {
            let voices = match voices {
                Some(v) => {
                    let (a, b) = v.split_once(',').context("--voices O,F")?;
                    Some((a.trim().to_string(), b.trim().to_string()))
                }
                None => None,
            };
            let o = podcast::Opts { focus, len: podcast::Len::parse(&len)?, voices, lang, script_only, from_script, loudness: loudness.map(|l| l.parse().map_err(anyhow::Error::msg)).transpose()?, no_disclosure, fresh, jingle: (jingle || no_jingle).then_some(!no_jingle), bed: bed.then_some(true) };
            json!(podcast::run(&st, &st.get(&nb)?, &o, &prog)?)
        }
        Cmd::Reroll { nb, line, dir, undo } => podcast::reroll::reroll(&st, &st.get(&nb)?, Some(&dir), line, undo, &prog)?,
        Cmd::Music { cmd } => match cmd {
            MusicCmd::Make { seed, prompt, force } => {
                let b = funkhaus_core::music::make(&st, seed, prompt.as_deref(), force, &prog)?;
                json!(b.iter().map(|b| json!({"part": b.part, "key": b.key, "cached": b.cached, "path": b.path})).collect::<Vec<_>>())
            }
            MusicCmd::Show => funkhaus_core::music::show(&st),
        },
        Cmd::Podcasts { nb } => json!(podcast::list(&st, &st.get(&nb)?)),
        Cmd::Map { nb, source, fast } => funkhaus_core::map::run(&st, &st.get(&nb)?, source.as_deref(), fast, &prog)?,
        Cmd::Guide { nb, source } => funkhaus_core::guide::run(&st, &st.get(&nb)?, source.as_deref(), &prog)?,
        Cmd::EvalGuide { nb } => {
            let cs = funkhaus_core::guide::eval(&st, &st.get(&nb)?)?;
            for c in &cs {
                println!("{}", serde_json::to_string(c)?);
            }
            let pass = cs.iter().filter(|c| c.pass).count();
            eprintln!("{pass}/{} Prüfungen bestanden", cs.len());
            std::process::exit(0);
        }
        Cmd::EvalPodcast { nb, dir } => {
            let n = st.get(&nb)?;
            let d = if dir.is_absolute() || dir.exists() { dir } else { st.out_dir(&n).join(dir) };
            let cs = podcast::staged::eval(&d)?;
            for c in &cs {
                println!("{}", serde_json::to_string(c)?);
            }
            eprintln!("{}/{} Prüfungen bestanden", cs.iter().filter(|c| c.pass).count(), cs.len());
            std::process::exit(0);
        }
        Cmd::Eval { nb, set, k, ranker } => {
            let ks: Vec<usize> = k.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>().context("--k z. B. 5,10")?;
            funkhaus_core::eval::eval(&st, &st.get(&nb)?, &set, &ks, &ranker)?
        }
        Cmd::Lex { nb, cmd } => {
            let p = st.dir(&st.get(&nb)?.id).join("lexicon.tsv");
            let row = |r: &funkhaus_voice::lexicon::Row| json!({"key": r.key, "speakable": r.speakable, "note": r.note});
            match cmd {
                LexCmd::Ls => json!(funkhaus_voice::lexicon::rows(&p).iter().map(row).collect::<Vec<_>>()),
                LexCmd::Add { key, speakable, note } => json!({"added": row(&funkhaus_voice::lexicon::add(&p, &key, &speakable, &note)?)}),
                LexCmd::Rm { key } => {
                    funkhaus_voice::lexicon::rm(&p, &key)?;
                    json!({"removed": key})
                }
            }
        }
        Cmd::Voice { cmd } => match cmd {
            VoiceCmd::Ls => funkhaus_core::voices::ls(&st),
            VoiceCmd::Show { name } => funkhaus_core::voices::show(&st, &name)?,
            VoiceCmd::Add { name, kind, voice, instruct, ref_, ref_text, lang, seed, description, consent_who, consent_statement, consent_date, consent_recording } => {
                use funkhaus_voice::persona::Kind;
                let kind = Some(match kind.as_str() { "builtin" => Kind::Builtin, "design" => Kind::Design, _ => Kind::Clone });
                funkhaus_core::voices::add(&st, &name, funkhaus_core::voices::New { kind, voice, instruct, ref_, ref_text, lang, seed, description, consent_who, consent_statement, consent_date, consent_recording })?
            }
            VoiceCmd::Test { name, text } => json!({"wav": funkhaus_core::voices::test_real(&st, &name, &text, &prog)?}),
        },
        Cmd::Doctor => {
            let (ok, v) = doctor::run(&st);
            if !ok {
                println!("{}", serde_json::to_string_pretty(&v)?);
                std::process::exit(1);
            }
            v
        }
    })
}

fn main() {
    util::fix_path();
    let cli = Cli::parse();
    let res = std::panic::catch_unwind(|| run(cli.cmd)).unwrap_or_else(|p| {
        let m = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
        Err(anyhow::anyhow!("interner Fehler: {m}"))
    });
    match res {
        Ok(v) => {
            let v = if v.get("ok").is_some() { v } else { json!({"ok": true, "result": v}) };
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
        }
        Err(e) => {
            let msg = format!("{e:#}");
            eprintln!("Fehler: {msg}");
            println!("{}", json!({"ok": false, "error": msg}));
            std::process::exit(1);
        }
    }
}
