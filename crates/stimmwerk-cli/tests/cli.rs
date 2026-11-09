//! JSON contract: one JSON object on stdout, exit 0 ok / 1 failed / 2 usage.
use serde_json::Value;
use std::process::Command;

fn run(home: &std::path::Path, args: &[&str]) -> (i32, Value) {
    let o = Command::new(env!("CARGO_BIN_EXE_stimmwerk")).args(args).env("STIMMWERK_HOME", home).env("FUNKHAUS_HOME", home.join("fh")).output().unwrap();
    let v = serde_json::from_slice(&o.stdout).unwrap_or(Value::Null);
    (o.status.code().unwrap(), v)
}

#[test]
fn new_import_open_lint() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path();
    let (c, v) = run(h, &["new", "hafen", "--title", "Am Hafen"]);
    assert_eq!((c, v["ok"].as_bool()), (0, Some(true)), "{v}");
    assert!(h.join("projects/hafen.studio/project.toml").is_file());
    assert_eq!(run(h, &["new", "hafen"]).0, 1);
    let fix = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/project/dialog.txt");
    let (c, v) = run(h, &["script", "import", "hafen", fix, "--detect"]);
    assert_eq!(c, 0, "{v}");
    assert_eq!(v["result"]["import"]["lines"], 8);
    assert_eq!(v["result"]["project"]["speakers"].as_array().unwrap().len(), 4);
    let (c, v) = run(h, &["open", "hafen"]);
    assert_eq!((c, v["result"]["title"].as_str()), (0, Some("Am Hafen")));
    let (c, v) = run(h, &["lint", "hafen"]);
    assert_eq!((c, v["result"]["errors"].as_u64()), (0, Some(0)), "{v}");
    std::fs::write(h.join("projects/hafen.studio/script.txt"), "Otto|0|Hallo.\n").unwrap();
    let (c, v) = run(h, &["lint", "hafen"]);
    assert_eq!((c, v["ok"].as_bool(), v["result"]["issues"][0]["code"].as_str()), (1, Some(false), Some("unknown_speaker")));
    assert_eq!(run(h, &["open", "nichtda"]).0, 1);
    let (c, v) = run(h, &["speaker", "set", "hafen", "Otto", "--persona", "gt", "--label", "Otto K."]);
    assert_eq!(c, 0, "{v}");
    let otto = |v: &Value| v["result"].as_array().unwrap().iter().find(|s| s["id"] == "Otto").cloned().unwrap();
    assert_eq!((otto(&v)["kind"].as_str(), otto(&v)["persona"].as_str()), (Some("implicit"), Some("gt")));
    assert_eq!(run(h, &["lint", "hafen"]).0, 0);
    assert_eq!(run(h, &["speaker", "set", "hafen", "B", "--persona", "gt"]).0, 1);
    let (c, v) = run(h, &["speaker", "ls", "hafen"]);
    assert_eq!((c, otto(&v)["label"].as_str(), otto(&v)["lines"].as_u64()), (0, Some("Otto K."), Some(1)));
    assert_eq!(run(h, &["bogus"]).0, 2);
}

/// `project ls|mv|trash`: JSON list, rename follows index + gui.json, trash needs --yes (exit 2) and goes to
/// STIMMWERK_TRASH_DIR instead of the real Trash.
#[test]
fn project_ls_mv_trash() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path();
    let bin = h.join("bin");
    let runt = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_stimmwerk")).args(args).env("STIMMWERK_HOME", h).env("FUNKHAUS_HOME", h.join("fh")).env(stimmwerk_core::project::TRASH_DIR_ENV, &bin).output().unwrap();
        (o.status.code().unwrap(), serde_json::from_slice::<Value>(&o.stdout).unwrap_or(Value::Null), String::from_utf8_lossy(&o.stderr).into_owned())
    };
    assert_eq!(runt(&["project", "ls"]).1["result"].as_array().map(Vec::len), Some(0));
    run(h, &["new", "b-zwei"]);
    run(h, &["new", "a-eins", "--title", "Eins"]);
    let d = h.join("projects/a-eins.studio");
    std::fs::write(d.join("script.txt"), "ERZ|0|Hallo.\nERZ|0|Welt.\n").unwrap();
    let wav = d.join("takes/x.wav");
    std::fs::write(&wav, b"x").unwrap();
    let ext = h.join("eigener-export.wav");
    std::fs::write(&ext, b"e").unwrap();
    std::fs::write(d.join("takes/voices.tsv"), format!("sha\t{}\t1\t1\tp\nsha\t{}\t1\t1\tp\n", wav.display(), ext.display())).unwrap();
    std::fs::write(h.join("gui.json"), format!("{{\"recent\": [\"{}\"]}}", d.display())).unwrap();
    let (c, v, _) = runt(&["project", "ls"]);
    let l = v["result"].as_array().unwrap();
    assert_eq!((c, l.len(), l[0]["name"].as_str(), l[0]["title"].as_str(), l[0]["lines"].as_u64(), l[0]["sync"].as_bool()), (0, 2, Some("a-eins"), Some("Eins"), Some(2), Some(false)), "{v}");
    assert!(l[0]["size"].as_u64().unwrap() > 0 && l[0]["modified"].as_str().unwrap().starts_with("20") && l[0]["path"].as_str().unwrap().ends_with("a-eins.studio"));
    // mv: refused for taken names, otherwise folder + title + index + gui.json follow
    assert_eq!(runt(&["project", "mv", "a-eins", "b-zwei"]).0, 1);
    assert_eq!(runt(&["project", "mv", "a-eins", "x/y"]).0, 1);
    let (c, v, _) = runt(&["project", "mv", "a-eins", "drei"]);
    let n = h.join("projects/drei.studio");
    assert_eq!((c, v["result"]["name"].as_str(), v["result"]["title"].as_str()), (0, Some("drei"), Some("drei")), "{v}");
    assert!(n.is_dir() && !d.exists());
    assert!(std::fs::read_to_string(n.join("takes/voices.tsv")).unwrap().contains(&n.join("takes/x.wav").display().to_string()));
    assert!(std::fs::read_to_string(h.join("gui.json")).unwrap().contains("drei.studio"));
    // trash: --yes required (exit 2, nothing moved), then into the fake trash dir; gui.json forgets it
    let (c, v, e) = runt(&["project", "trash", "drei"]);
    assert_eq!((c, v), (2, Value::Null), "{e}");
    assert!(e.contains("--yes") && n.is_dir());
    assert_eq!(runt(&["project", "trash", "nichtda", "--yes"]).0, 1);
    let (c, v, e) = runt(&["project", "trash", "drei", "--yes"]);
    assert_eq!((c, v["result"]["name"].as_str()), (0, Some("drei")), "{v}");
    assert!(!n.exists() && bin.join("drei.studio/takes/x.wav").is_file());
    // the elsewhere-export went along (out/extern), its line follows; the note about the Trash is printed
    assert!(!ext.exists() && bin.join("drei.studio/out/extern/eigener-export.wav").is_file(), "{e}");
    assert_eq!(v["result"]["moved_in"][0].as_str(), ext.to_str(), "{v}");
    assert!(std::fs::read_to_string(bin.join("drei.studio/takes/voices.tsv")).unwrap().contains(&n.join("out/extern/eigener-export.wav").display().to_string()));
    assert!(e.contains("Papierkorb danach leeren") && e.contains("eigener-export.wav"), "{e}");
    assert!(!std::fs::read_to_string(h.join("gui.json")).unwrap().contains("drei.studio"));
    assert_eq!(runt(&["project", "ls"]).1["result"].as_array().map(Vec::len), Some(1));
}

/// 16-bit mono 24 kHz WAV of `n` samples at a constant level.
fn exec(p: &std::path::Path) {
    std::fs::set_permissions(p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

fn wav(n: usize) -> Vec<u8> {
    let d = (n * 2) as u32;
    let mut v = [b"RIFF".as_slice(), &(36 + d).to_le_bytes(), b"WAVEfmt ", &16u32.to_le_bytes(), &1u16.to_le_bytes(), &1u16.to_le_bytes(), &24_000u32.to_le_bytes(), &48_000u32.to_le_bytes(), &2u16.to_le_bytes(), &16u16.to_le_bytes(), b"data", &d.to_le_bytes()].concat();
    (0..n).for_each(|_| v.extend(3000i16.to_le_bytes()));
    v
}

#[test]
fn render_reroll_with_fake_tts() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path();
    std::fs::write(h.join("tone.wav"), wav(4800)).unwrap();
    let fake = h.join("fake-omnivoice");
    std::fs::write(&fake, format!("#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift;; esac; shift; done\necho \"$out\" >> '{0}/calls'\ncp '{0}/tone.wav' \"$out\"\n", h.display())).unwrap();
    exec(&fake);
    let fix = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/project/leuchtturm.studio");
    let d = h.join("projects/l.studio");
    std::fs::create_dir_all(&d).unwrap();
    for f in ["project.toml", "script.txt", "lexicon.tsv"] {
        std::fs::copy(std::path::Path::new(fix).join(f), d.join(f)).unwrap();
    }
    let go = |args: &[&str]| {
        // HOME too: the GPU lock dir lives under $HOME
        let o = Command::new(env!("CARGO_BIN_EXE_stimmwerk")).args(args).env("STIMMWERK_HOME", h).env("FUNKHAUS_HOME", h.join("fh")).env("HOME", h).env("STIMMWERK_TTS_CMD", &fake).output().unwrap();
        (o.status.code().unwrap(), serde_json::from_slice::<Value>(&o.stdout).unwrap_or(Value::Null))
    };
    let calls = || std::fs::read_to_string(h.join("calls")).unwrap_or_default().lines().count();
    let (c, v) = go(&["render", "l"]);
    assert_eq!(c, 0, "{v}");
    let r = &v["result"];
    assert_eq!((r["lines"].as_u64(), r["tts_calls"].as_u64(), r["cached"].as_u64(), calls()), (Some(7), Some(8), Some(0), 8));
    assert!(d.join("out/mix.wav").is_file() && r["secs"].as_f64().unwrap() > 1.0);
    assert!(h.join("Library/Application Support/funkhaus/.gpu.lock").exists());
    let mix0 = std::fs::read(d.join("out/mix.wav")).unwrap();
    let (c, v) = go(&["render", "l"]);
    assert_eq!((c, v["result"]["tts_calls"].as_u64(), calls()), (0, Some(0), 8));
    let (c, v) = go(&["reroll", "l", "2"]);
    assert_eq!((c, v["result"]["take"].as_u64(), v["result"]["render"]["rendered"][0].as_u64(), calls()), (0, Some(1), Some(2), 9));
    let (c, v) = go(&["reroll", "l", "2", "--undo"]);
    assert_eq!((c, v["result"]["take"].as_u64(), calls()), (0, Some(0), 9));
    assert_eq!(std::fs::read(d.join("out/mix.wav")).unwrap(), mix0);
    let (c, v) = go(&["render", "l", "--line", "4", "--quality", "draft"]);
    assert_eq!((c, v["result"]["tts_calls"].as_u64(), v["result"]["quality"].as_str()), (0, Some(2), Some("draft")), "{v}");
    assert!(d.join("out/line.wav").is_file() && d.join("takes/draft").is_dir());
    assert_eq!(go(&["render", "l", "--line", "99"]).0, 1);
    assert_eq!(go(&["reroll", "l", "1", "--undo"]).0, 1);
    // export: final takes cached, only the spoken disclosure is new
    let (c, v) = go(&["export", "l", "parts", "--loudness", "podcast"]);
    let r = &v["result"];
    assert_eq!((c, r["tts_calls"].as_u64(), r["parts"].as_u64(), r["preset"].as_str(), calls()), (0, Some(1), Some(8), Some("podcast"), 12), "{v}");
    assert!(r["measured"]["lufs"].as_f64().is_some() && d.join("out/manifest.json").is_file() && d.join("out/parts/08-ERZ.wav").is_file());
    let (c, v) = go(&["export", "l", "mp3", "--no-disclosure"]);
    assert_eq!((c, v["result"]["ai"]["disclosed"].as_bool(), v["result"]["preset"].as_str(), calls()), (0, Some(false), Some("hoerbuch"), 12), "{v}");
    assert_eq!(go(&["export", "l", "flac"]).0, 2);
    assert_eq!(go(&["export", "l", "wav", "--loudness", "laut"]).0, 2);
}

/// `voice` JSON contract with a fake omnivoice that logs its args (voice-tests.md).
#[test]
fn voice_design_keep_clone_revoke() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path();
    std::fs::write(h.join("tone.wav"), wav(24_000 * 4)).unwrap();
    let fake = h.join("fake-omnivoice");
    let script = "#!/bin/sh\nif [ \"$1\" = voices ]; then echo '{\"voices\":[{\"name\":\"gt\"}]}'; exit 0; fi\necho \"$@ cache=$OMNIVOICE_CACHE\" >> '{0}/calls'\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift;; esac; shift; done\ncp '{0}/tone.wav' \"$out\"\n";
    std::fs::write(&fake, script.replace("{0}", &h.display().to_string())).unwrap();
    exec(&fake);
    let go = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_stimmwerk")).args(args).env("STIMMWERK_HOME", h).env("FUNKHAUS_HOME", h.join("fh")).env("HOME", h).env("STIMMWERK_TTS_CMD", &fake).output().unwrap();
        (o.status.code().unwrap(), serde_json::from_slice::<Value>(&o.stdout).unwrap_or(Value::Null))
    };
    let calls = || std::fs::read_to_string(h.join("calls")).unwrap_or_default();
    let (c, v) = go(&["voice", "presets"]);
    assert_eq!((c, v["result"].as_array().map(Vec::len)), (0, Some(20)));
    // design: 3 seeds, candidates.json, no persona
    let (c, v) = go(&["voice", "design", "--preset", "1", "--text", "Hallo du."]);
    assert_eq!(c, 0, "{v}");
    let b = &v["result"];
    let batch = b["batch"].as_str().unwrap().to_string();
    assert_eq!((b["instruct"].as_str(), b["candidates"].as_array().unwrap().len()), (Some("male, middle-aged, low pitch"), 3));
    assert!(h.join("voices/candidates").join(&batch).join("candidates.json").is_file());
    for s in 1..=3 {
        assert!(calls().contains(&format!("--instruct male, middle-aged, low pitch --seed {s} ")), "{}", calls());
    }
    assert_eq!(go(&["voice", "design", "--instruct", "female", "--n", "0"]).0, 1);
    assert_eq!(go(&["voice", "design", "--instruct", "female", "--n", "abc"]).0, 2);
    assert_eq!(go(&["voice", "design"]).0, 1);
    // keep -> persona, then probe renders via --ref (anchor), cached on repeat
    let (c, v) = go(&["voice", "keep", &batch, "2", "--name", "erz"]);
    assert_eq!((c, v["result"]["seed"].as_u64(), v["result"]["provenance"]["license"].as_str()), (0, Some(2), Some("LicenseRef-own")), "{v}");
    assert_eq!(go(&["voice", "keep", &batch, "1", "--name", "roh", "--no-anchor"]).0, 2, "--no-anchor is gone");
    assert_eq!(go(&["voice", "keep", &batch, "2", "--name", "erz"]).0, 1);
    assert_eq!(go(&["voice", "keep", &batch, "9", "--name", "neu"]).0, 1);
    let (c, v) = go(&["voice", "probe", "erz", "--text", "Probe."]);
    assert_eq!((c, v["result"]["cache_hit"].as_bool(), v["result"]["spec"]["kind"].as_str()), (0, Some(false), Some("design")), "{v}");
    assert!(calls().lines().last().unwrap().contains("--ref "));
    assert!(calls().lines().last().unwrap().ends_with(&format!("cache={}", h.join("cache/omnivoice").display())), "{}", calls());
    assert_eq!(go(&["voice", "probe", "erz", "--text", "Probe."]).1["result"]["cache_hit"], true);
    // clone: consent required (exit 1, nothing written)
    let src = h.join("ref.wav");
    std::fs::write(&src, wav(24_000 * 6)).unwrap();
    let s = src.to_str().unwrap();
    let (c, v) = go(&["voice", "clone", s, "--name", "ich", "--ref-text", "Hallo."]);
    assert_eq!((c, v["ok"].as_bool()), (1, Some(false)));
    assert!(v["error"].as_str().unwrap().contains("Einwilligung fehlt"));
    assert!(!h.join("voices/ich.toml").exists() && !h.join("voices/clips/ich.wav").exists());
    let (c, v) = go(&["voice", "clone", s, "--name", "ich", "--ref-text", "Hallo.", "--consent", "Eigene synthetische Stimme, Test"]);
    assert!(c == 1 && v["error"].as_str().unwrap().contains("--who"), "{v}");
    let (c, v) = go(&["voice", "clone", s, "--name", "ich", "--ref-text", "Hallo.", "--start", "1", "--end", "5", "--consent", "Eigene synthetische Stimme, Test", "--who", "Testperson"]);
    assert_eq!(v["result"]["provenance"]["license"], "LicenseRef-consent");
    assert_eq!((c, v["result"]["ref_sha256"].as_str().map(str::len)), (0, Some(64)), "{v}");
    let (c, v) = go(&["voice", "probe", "ich", "--text", "Probe."]);
    assert_eq!(c, 0, "{v}");
    let last = calls().lines().last().unwrap().to_string();
    assert!(last.contains("--ref ") && last.contains("clips/ich.wav") && !last.contains("voices add"), "{last}");
    // revoke -> render refused, visible in ls, clip + probe deleted
    let (c, v) = go(&["voice", "revoke", "ich"]);
    assert_eq!((c, v["result"]["already"].as_bool()), (0, Some(false)));
    let del: Vec<&str> = v["result"]["deleted"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
    assert!(del.iter().any(|d| d.ends_with("clips/ich.wav")) && del.iter().any(|d| d.contains("cache/probe/")), "{del:?}");
    assert!(!h.join("voices/clips/ich.wav").exists());
    let (c, v) = go(&["voice", "probe", "ich", "--text", "Noch mal."]);
    assert_eq!(c, 1);
    assert!(v["error"].as_str().unwrap().contains("widerrufen"), "{v}");
    assert!(!v["error"].as_str().unwrap().contains("funkhaus voice"));
    assert_eq!(go(&["voice", "revoke", "erz"]).0, 1);
    assert_eq!(go(&["voice", "rm", "ich"]).0, 1);
    let (c, v) = go(&["voice", "ls"]);
    assert_eq!(c, 0);
    let p = v["result"]["personas"].as_array().unwrap();
    let ich = p.iter().find(|x| x["name"] == "ich").unwrap();
    assert_eq!((ich["kind"].as_str(), ich["valid"].as_bool(), ich["revoked"].is_string()), (Some("clone"), Some(false), true));
    assert_eq!(v["result"]["builtins"][0], "gt");
    let (c, v) = go(&["voice", "rm", "erz"]);
    assert_eq!(c, 0, "{v}");
    assert!(!h.join("voices/erz.toml").exists() && !h.join("voices/clips/erz.wav").exists());
    assert!(go(&["voice", "probe", "nichtda-x"]).1["error"].as_str().is_none_or(|e| !e.contains("funkhaus voice")));
}

/// `sync`: exit 0 all fit/atempo, 1 too_long/overlap (full report, ok:false) and errors, 2 usage.
#[test]
fn sync_exit_codes() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path();
    let fx = |f: &str| format!("{}/../../fixtures/stimmwerk/sync/{f}", env!("CARGO_MANIFEST_DIR"));
    let go = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_stimmwerk")).args(args).env("STIMMWERK_HOME", h).env("FUNKHAUS_HOME", h.join("fh")).env("HOME", h).env("STIMMWERK_TTS_CMD", "/usr/bin/false").output().unwrap();
        (o.status.code().unwrap(), serde_json::from_slice::<Value>(&o.stdout).unwrap_or(Value::Null))
    };
    let st = |v: &Value| v["result"]["cues"].as_array().unwrap().iter().map(|c| c["status"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    // plan form without a project, and `sync check <file>`
    let (c, v) = go(&["sync", &fx("fit3.srt")]);
    assert_eq!((c, v["ok"].as_bool(), st(&v)), (1, Some(false), vec!["fits".to_string(), "atempo".into(), "too_long".into()]), "{v}");
    let c3 = &v["result"]["cues"][2];
    assert!(c3["duration"].is_f64() && c3["target"] == 2.0 && c3["factor"].as_f64().unwrap() > 1.12, "{c3}");
    assert_eq!(go(&["sync", &fx("fit3.vtt")]).1["result"]["cues"], v["result"]["cues"]);
    let (c, v) = go(&["sync", "check", &fx("ok2.srt")]);
    assert_eq!((c, v["ok"].as_bool(), v["result"]["summary"]["ok"].as_bool()), (0, Some(true), Some(true)), "{v}");
    let (c, v) = go(&["sync", "check", &fx("edge.srt")]);
    assert_eq!((c, st(&v)[..2].to_vec()), (1, vec!["overlap".to_string(), "overlap".into()]));
    // project
    assert_eq!(go(&["new", "s"]).0, 0);
    let (c, v) = go(&["sync", "import", "s", &fx("ok2.srt")]);
    assert_eq!((c, v["result"]["import"]["cues"].as_u64(), v["result"]["project"]["sync"].as_bool()), (0, Some(2), Some(true)), "{v}");
    assert_eq!(go(&["sync", "check", "s"]).0, 0);
    let (c, v) = go(&["sync", "import", "s", &fx("fit3.srt")]);
    assert!(c == 1 && v["ok"] == false && v["error"].as_str().unwrap().contains("--replace"), "{v}");
    let (c, v) = go(&["sync", "import", "s", &fx("fit3.srt"), "--replace"]);
    assert_eq!((c, v["result"]["check"]["summary"]["ok"].as_bool()), (0, Some(false)), "import itself succeeds: {v}");
    let (c, v) = go(&["sync", "check", "s"]);
    assert_eq!((c, v["result"]["cues"][2]["status"].as_str()), (1, Some("too_long")));
    let (c, v) = go(&["sync", "render", "s"]);
    assert!(c == 1 && v["error"].as_str().unwrap().contains("ERZ"), "{v}");
    assert_eq!(go(&["sync", "render", "nichtda"]).0, 1);
    let (c, v) = go(&["sync", "check", "nichtda"]);
    assert!(c == 1 && v["ok"] == false && v["error"].is_string());
    assert_eq!(go(&["new", "leer"]).0, 0);
    assert!(go(&["sync", "check", "leer"]).1["error"].as_str().unwrap().contains("kein Sync-Projekt"));
    assert_eq!(go(&["sync", "bogus"]).0, 2);
    assert_eq!(go(&["sync"]).0, 2);
    // plan form: an existing file without .srt/.vtt extension is sniffed (SRT here)
    let noext = h.join("untertitel");
    std::fs::copy(fx("fit3.srt"), &noext).unwrap();
    let (c, v) = go(&["sync", noext.to_str().unwrap()]);
    assert_eq!((c, st(&v)), (1, vec!["fits".to_string(), "atempo".into(), "too_long".into()]), "{v}");
    assert_eq!(v["result"]["cues"][0]["who"].as_str(), Some("ERZ"));
    assert_eq!(go(&["sync", "render", "s", "--quality", "mittel"]).0, 2);
}

/// `sync render|reroll` and `export` on a sync project with a fake TTS (0.2 s tone per cue): reroll bumps the take and
/// re-renders in the sync namespace; export is gated on the fit (`--allow-too-long`) and ends 0.5 s after the disclosure.
#[test]
fn sync_reroll_and_export_allow_too_long() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path();
    std::fs::write(h.join("tone.wav"), wav(4800)).unwrap();
    let fake = h.join("fake-omnivoice");
    std::fs::write(&fake, format!("#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift;; esac; shift; done\necho \"$out\" >> '{0}/calls'\ncp '{0}/tone.wav' \"$out\"\n", h.display())).unwrap();
    exec(&fake);
    let ok = h.join("ok.srt");
    std::fs::write(&ok, "1\n00:00:00,000 --> 00:00:01,000\nHallo.\n\n2\n00:00:02,000 --> 00:00:03,000\nWelt.\n").unwrap();
    let long = h.join("long.srt");
    std::fs::write(&long, "1\n00:00:00,000 --> 00:00:00,100\nHallo.\n\n2\n00:00:02,000 --> 00:00:03,000\nWelt.\n").unwrap();
    let go = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_stimmwerk")).args(args).env("STIMMWERK_HOME", h).env("FUNKHAUS_HOME", h.join("fh")).env("HOME", h).env("STIMMWERK_TTS_CMD", &fake).output().unwrap();
        (o.status.code().unwrap(), serde_json::from_slice::<Value>(&o.stdout).unwrap_or(Value::Null))
    };
    let calls = || std::fs::read_to_string(h.join("calls")).unwrap_or_default().lines().count();
    let st = |v: &Value| v["cues"].as_array().unwrap().iter().map(|c| c["status"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(go(&["new", "s"]).0, 0);
    assert_eq!(go(&["sync", "import", "s", ok.to_str().unwrap()]).0, 0);
    assert_eq!(go(&["speaker", "set", "s", "ERZ", "--persona", "gt"]).0, 0);
    let _ = std::fs::remove_file(h.join("calls")); // `speaker set` may ask the TTS for its voice list
    let (c, v) = go(&["sync", "render", "s"]);
    assert_eq!((c, st(&v["result"]), v["result"]["render"]["tts_calls"].as_u64(), calls()), (0, vec!["fits".to_string(), "fits".into()], Some(2), 2), "{v}");
    let wav0 = std::fs::read(h.join("projects/s.studio/out/sync.wav")).unwrap();
    // reroll: one new take, full re-render from cache otherwise; undo restores the mix bit-identically
    let (c, v) = go(&["sync", "reroll", "s", "2"]);
    let r = &v["result"];
    assert_eq!((c, r["cue"].as_u64(), r["take"].as_u64(), r["undo"].as_bool(), calls()), (0, Some(2), Some(1), Some(false), 3), "{v}");
    assert_eq!((r["report"]["render"]["rendered"][0].as_u64(), r["report"]["render"]["cached"].as_u64(), st(&r["report"])), (Some(2), Some(1), vec!["fits".to_string(), "fits".into()]));
    assert!(h.join("projects/s.studio/takes/sync/rerolls.json").is_file());
    let (c, v) = go(&["sync", "reroll", "s", "2", "--undo"]);
    assert_eq!((c, v["result"]["take"].as_u64(), calls()), (0, Some(0), 3), "{v}");
    assert_eq!(std::fs::read(h.join("projects/s.studio/out/sync.wav")).unwrap(), wav0);
    assert_eq!(go(&["sync", "reroll", "s", "9"]).0, 1);
    assert_eq!(go(&["sync", "reroll", "s", "2", "--undo"]).0, 1, "nothing to undo");
    assert_eq!(go(&["sync", "reroll", "s"]).0, 2);
    // export: cues at their times, disclosure 1 s after the last cue end (3 s) + 0.2 s tone + 0.5 s tail
    let (c, v) = go(&["export", "s", "wav"]);
    let r = &v["result"];
    assert_eq!((c, r["tts_calls"].as_u64(), r["sync"]["ok"].as_bool(), r["ai"]["disclosed"].as_bool(), calls()), (0, Some(1), Some(true), Some(true), 4), "{v}");
    assert!((r["duration"].as_f64().unwrap() - 4.7).abs() < 0.03, "{v}");
    let (c, v) = go(&["export", "s", "wav", "--no-disclosure"]);
    assert_eq!((c, v["result"]["tts_calls"].as_u64()), (0, Some(0)), "{v}");
    assert!((v["result"]["duration"].as_f64().unwrap() - 3.0).abs() < 0.03, "no tail without disclosure: {v}");
    // too_long cue: render exit 1 with the report, export refuses unless --allow-too-long
    assert_eq!(go(&["sync", "import", "s", long.to_str().unwrap(), "--replace"]).0, 0);
    let (c, v) = go(&["sync", "render", "s"]);
    assert_eq!((c, v["ok"].as_bool(), st(&v["result"])), (1, Some(false), vec!["too_long".to_string(), "fits".into()]), "{v}");
    assert!(v["result"]["cues"][0]["warning"].as_str().unwrap().contains("zu lang"));
    let (c, v) = go(&["sync", "reroll", "s", "1"]);
    assert_eq!((c, v["ok"].as_bool(), v["result"]["take"].as_u64()), (1, Some(false), Some(1)), "reroll reports too_long too: {v}");
    std::fs::remove_file(h.join("projects/s.studio/out/s.wav")).unwrap();
    let (c, v) = go(&["export", "s", "wav"]);
    assert!(c == 1 && v["error"].as_str().unwrap().contains("Cue 1") && v["error"].as_str().unwrap().contains("--allow-too-long"), "{v}");
    assert!(!h.join("projects/s.studio/out/s.wav").is_file(), "refused export must not write s.wav");
    let (c, v) = go(&["export", "s", "parts", "--allow-too-long", "--no-disclosure"]);
    let r = &v["result"];
    assert_eq!((c, r["sync"]["ok"].as_bool(), r["sync"]["too_long"].as_u64(), r["parts"].as_u64()), (0, Some(false), Some(1), Some(2)), "{v}");
    let man: Value = serde_json::from_str(&std::fs::read_to_string(h.join("projects/s.studio/out/manifest.json")).unwrap()).unwrap();
    assert_eq!((man["sync"]["ok"].as_bool(), man["paragraphs"][0]["status"].as_str()), (Some(false), Some("too_long")), "{man}");
}

/// `llm ls|add|rm|test|use|key`, `prose`, `sync shorten`: mock LLM on loopback, fake keychain, no key in any file,
/// online providers refused without both switches and never contacted.
#[test]
fn llm_prose_shorten_contract() {
    use funkhaus_llm::testsrv::{Mock, fixture, ok};
    let t = tempfile::tempdir().unwrap();
    let h = t.path();
    let fake = funkhaus_llm::keychain::fake(h);
    let go = |args: &[&str], stdin: &str| {
        let mut ch = Command::new(env!("CARGO_BIN_EXE_stimmwerk")).args(args).env("STIMMWERK_HOME", h).env("FUNKHAUS_HOME", h.join("fh")).env("HOME", h).env("FUNKHAUS_SECURITY", &fake).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        std::io::Write::write_all(&mut ch.stdin.take().unwrap(), stdin.as_bytes()).unwrap();
        let o = ch.wait_with_output().unwrap();
        (o.status.code().unwrap(), serde_json::from_slice::<Value>(&o.stdout).unwrap_or(Value::Null), String::from_utf8_lossy(&o.stderr).into_owned())
    };
    let (c, v, _) = go(&["llm", "ls"], "");
    assert_eq!((c, v["result"]["default"].as_str(), v["result"]["backends"].as_array().unwrap().len(), v["result"]["presets"].as_array().unwrap().len()), (0, Some("gemma"), 4, 7), "{v}");
    assert!(!h.join("settings.toml").exists(), "ls writes nothing");
    // local mock backend becomes the default
    let m = Mock::start(vec![ok(r#"{"data":[{"id":"m"}]}"#)]);
    let (c, v, _) = go(&["llm", "add", "mock", "--url", &format!("{}/v1", m.url()), "--model", "m", "--default"], "");
    assert_eq!((c, v["result"]["local"].as_bool(), v["result"]["default"].as_bool()), (0, Some(true), Some(true)), "{v}");
    let (c, v, _) = go(&["llm", "test", "mock"], "");
    assert_eq!((c, v["result"]["models"][0].as_str(), v["result"]["model_available"].as_bool(), m.hits()), (0, Some("m"), Some(true), 1), "{v}");
    assert_eq!(go(&["llm", "add", "neu"], "").0, 1, "no url, no preset");
    assert_eq!(go(&["llm", "add", "a b", "--url", "http://127.0.0.1:1/v1"], "").0, 1);
    // prose against the mock (kapitel1 fixture reply)
    let m = Mock::start(vec![fixture("openai/200_success")]);
    assert_eq!(go(&["llm", "add", "mock", "--url", &format!("{}/v1", m.url())], "").0, 0);
    assert_eq!(go(&["new", "k", "--title", "Kiesbucht"], "").0, 0);
    let fix = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/prosa/kapitel1.md");
    let (c, v, err) = go(&["prose", "k", fix], "");
    let r = &v["result"]["prose"];
    assert_eq!((c, r["lines"].as_u64(), r["online"].as_bool(), r["speakers_added"].as_array().unwrap().len(), r["cast_merged"].as_array().unwrap().len(), m.hits()), (0, Some(37), Some(false), 4, 0, 1), "{v}");
    assert!(r["cast"][1]["suggested_voice_instruct"].as_str().unwrap().contains("female") && err.contains("voice design --instruct"), "{err}");
    let d = h.join("projects/k.studio");
    let sc = std::fs::read_to_string(d.join("script.txt")).unwrap();
    assert_eq!(sc.lines().count(), 37, "3 reply lines hold two sentences");
    assert!(sc.contains("Jonas|0.5|Er hat mich gefunden. [laughter]\n") && sc.contains("Am 3. Oktober 1964"), "{sc}");
    let pt = std::fs::read_to_string(d.join("project.toml")).unwrap();
    assert!(pt.contains("[speakers.Ilse]") && pt.contains("label = \"Erzähler\"") && !pt.contains("url") && !pt.contains("key"), "{pt}");
    let req = m.reqs().pop().unwrap().body_str();
    assert!(req.contains("Kiesbucht ist ein Dorf") && !req.contains("## Der Kai"), "headings dropped");
    assert_eq!(go(&["prose", "k", fix], "").0, 1, "non-empty script needs --replace");
    // online preset: refused at the project switch, then at the app switch, then needs --online-ack; never connected
    let (c, v, _) = go(&["llm", "add", "anthropic"], "");
    assert_eq!((c, v["result"]["kind"].as_str(), v["result"]["local"].as_bool(), v["result"]["allow_online"].as_bool(), v["result"]["key"].is_null()), (0, Some("anthropic"), Some(false), Some(false), true), "{v}");
    let (c, v, _) = go(&["prose", "k", fix, "--backend", "anthropic", "--replace"], "");
    assert!(c == 1 && v["error"].as_str().unwrap().contains("für dieses Projekt nicht freigegeben"), "{v}");
    let (c, v, _) = go(&["llm", "use", "k", "anthropic", "--online-ok"], "");
    assert_eq!((c, v["result"]["online_ok"].as_bool(), v["result"]["usable"].as_bool()), (0, Some(true), Some(false)), "{v}");
    assert!(v["result"]["blocked"].as_str().unwrap().contains("--allow-online"));
    let pt = std::fs::read_to_string(d.join("project.toml")).unwrap();
    assert!(pt.contains("[llm]\nbackend = \"anthropic\"\nonline_ok = true") && pt.contains("[speakers.Ilse]"), "{pt}");
    let (c, v, _) = go(&["prose", "k", fix, "--replace"], "");
    assert!(c == 1 && v["error"].as_str().unwrap().contains("Einstellungen nicht erlaubt"), "{v}");
    let (c, _, err) = go(&["llm", "add", "anthropic", "--allow-online"], "");
    assert!(c == 0 && err.contains("verlassen den Mac"), "{err}");
    let (c, v, _) = go(&["llm", "use", "k", "anthropic"], "");
    assert_eq!((c, v["result"]["usable"].as_bool()), (0, Some(true)), "{v}");
    let (c, v, err) = go(&["prose", "k", fix, "--replace"], "");
    assert!(c == 1 && v["error"].as_str().unwrap().contains("--online-ack") && err.contains("keine Personendaten"), "{v}");
    // key: stdin -> fake keychain only; settings carry the reference
    let (c, v, _) = go(&["llm", "key", "anthropic"], "sk-ant-api03-geheim\n");
    assert_eq!((c, v["result"]["key"].as_str()), (0, Some("keychain")), "{v}");
    let st = std::fs::read_to_string(h.join("settings.toml")).unwrap();
    assert!(st.contains("[llm.backends.anthropic.key.keychain]") && st.contains("account = \"anthropic\"") && st.contains("allow_online = true") && !st.contains("geheim"), "{st}");
    assert_eq!(std::fs::read_to_string(h.join("stimmwerk-llm--anthropic")).unwrap(), "sk-ant-api03-geheim");
    assert!(!std::fs::read_to_string(d.join("project.toml")).unwrap().contains("geheim"));
    let (c, v, _) = go(&["llm", "ls"], "");
    let cl = v["result"]["backends"].as_array().unwrap().iter().find(|b| b["name"] == "anthropic").unwrap().clone();
    assert_eq!((c, cl["key"].as_str(), cl["allow_online"].as_bool()), (0, Some("keychain"), Some(true)));
    assert_eq!(go(&["llm", "key", "anthropic"], "\n").0, 1, "empty key");
    assert_eq!(go(&["llm", "key", "anthropic", "--clear"], "").0, 0);
    assert!(!h.join("stimmwerk-llm--anthropic").exists() && !std::fs::read_to_string(h.join("settings.toml")).unwrap().contains("keychain"));
    let (c, v, _) = go(&["llm", "rm", "anthropic"], "");
    assert_eq!((c, v["result"]["default"].as_str()), (0, Some("mock")), "{v}");
    assert_eq!(go(&["llm", "rm", "anthropic"], "").0, 1);
    // local backend: `security` is never started (it would fail here); a backend with a key takes its item along
    let o = Command::new(env!("CARGO_BIN_EXE_stimmwerk")).args(["llm", "rm", "vmlx"]).env("STIMMWERK_HOME", h).env("FUNKHAUS_HOME", h.join("fh")).env("HOME", h).env("FUNKHAUS_SECURITY", "/nonexistent/security").output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(go(&["llm", "add", "anthropic"], "").0, 0);
    assert_eq!(go(&["llm", "key", "anthropic"], "sk-ant-x\n").0, 0);
    assert!(h.join("stimmwerk-llm--anthropic").exists());
    assert_eq!(go(&["llm", "rm", "anthropic"], "").0, 0);
    assert!(!h.join("stimmwerk-llm--anthropic").exists(), "key deleted with the backend");
    let (c, v, _) = go(&["prose", "k", fix, "--replace"], "");
    assert!(c == 1 && v["error"].as_str().unwrap().contains("nicht konfiguriert"), "project still names anthropic: {v}");
    assert_eq!(go(&["llm", "use", "k", "mock"], "").0, 0);
    assert_eq!(m.hits(), 1, "no request reached any server during the refusals");
    // sync shorten: proposal only, script untouched
    let m = Mock::start(vec![ok(r#"{"choices":[{"message":{"content":"{\"vorschlag\": \"Die Lieferung kommt morgen früh.\", \"zeichen\": 32, \"begruendung\": \"Zeitangabe doppelt.\"}"},"finish_reason":"stop"}]}"#)]);
    assert_eq!(go(&["llm", "add", "mock", "--url", &format!("{}/v1", m.url())], "").0, 0);
    let srt = h.join("l.srt");
    std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:02,500\nDie Lieferung kommt morgen früh am Vormittag.\n\n2\n00:00:03,000 --> 00:00:05,000\nGut.\n").unwrap();
    assert_eq!(go(&["new", "s"], "").0, 0);
    assert_eq!(go(&["sync", "import", "s", srt.to_str().unwrap()], "").0, 0);
    let script0 = std::fs::read(h.join("projects/s.studio/script.txt")).unwrap();
    let (c, v, err) = go(&["sync", "shorten", "s"], "");
    let r = &v["result"];
    assert_eq!((c, r["cues"].as_u64(), r["accepted"].as_u64(), r["suggestions"][0]["cue"].as_u64(), m.hits()), (0, Some(1), Some(1), Some(1), 1), "{v}");
    assert_eq!((r["suggestions"][0]["proposal"].as_str(), r["suggestions"][0]["target_chars"].as_u64()), (Some("Die Lieferung kommt morgen früh."), Some(33)));
    assert!(err.contains("Cue 1: Die Lieferung kommt morgen früh."), "{err}");
    assert_eq!(std::fs::read(h.join("projects/s.studio/script.txt")).unwrap(), script0, "never applied");
    let (c, v, _) = go(&["sync", "shorten", "s", "--cue", "2"], "");
    assert_eq!((c, v["result"]["cues"].as_u64(), v["result"]["accepted"].as_u64()), (0, Some(1), Some(0)), "numbers fine but band violated: hint only {v}");
    assert_eq!(go(&["sync", "shorten", "s", "--cue", "9"], "").0, 1);
    assert_eq!(go(&["sync", "shorten", "k"], "").0, 1, "no sync project");
}

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// `music gen|ls`, `cue add|ls|rm|import`, export with cues: fake acestep (logs its argv, copies a WAV), cache hit =
/// 0 calls, Ctrl-C while acestep "runs" ends within 2 s and leaves no cache file.
#[test]
fn music_and_cues_with_fake_acestep() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path();
    std::fs::write(h.join("tone.wav"), wav(4800)).unwrap();
    std::fs::write(h.join("music.wav"), wav(24_000 * 12)).unwrap();
    let fake_tts = h.join("fake-omnivoice");
    std::fs::write(&fake_tts, format!("#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift;; esac; shift; done\necho \"$out\" >> '{0}/calls'\ncp '{0}/tone.wav' \"$out\"\n", h.display())).unwrap();
    exec(&fake_tts);
    let fake_ace = h.join("fake-acestep");
    // `--version` is answered without a log line (asked once per process for the cache key); FAKE_SLEEP writes the pid
    std::fs::write(&fake_ace, format!("#!/bin/sh\n[ \"$1\" = --version ] && {{ echo 'acestep 0.0-fake'; exit 0; }}\necho \"$@\" >> '{0}/acalls'\n[ -n \"$FAKE_FAIL\" ] && exit 1\n[ -n \"$FAKE_SLEEP\" ] && {{ echo $$ > '{0}/ace.pid'; exec sleep \"$FAKE_SLEEP\"; }}\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift;; esac; shift; done\ncp '{0}/music.wav' \"$out\"\n", h.display())).unwrap();
    exec(&fake_ace);
    let fix = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/project/leuchtturm.studio");
    let d = h.join("projects/l.studio");
    std::fs::create_dir_all(&d).unwrap();
    for f in ["project.toml", "script.txt", "lexicon.tsv"] {
        std::fs::copy(std::path::Path::new(fix).join(f), d.join(f)).unwrap();
    }
    let cmd = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_stimmwerk"));
        c.args(args).env("STIMMWERK_HOME", h).env("FUNKHAUS_HOME", h.join("fh")).env("HOME", h).env("STIMMWERK_TTS_CMD", &fake_tts).env("STIMMWERK_ACESTEP_CMD", &fake_ace);
        c
    };
    let go = |args: &[&str]| {
        let o = cmd(args).output().unwrap();
        (o.status.code().unwrap(), serde_json::from_slice::<Value>(&o.stdout).unwrap_or(Value::Null), String::from_utf8_lossy(&o.stderr).into_owned())
    };
    let acalls = || std::fs::read_to_string(h.join("acalls")).unwrap_or_default();
    // gen: argv as documented, key + path, cache hit on repeat
    let (c, v, err) = go(&["music", "gen", "l", "--prompt", "calm maritime intro", "--dur", "12", "--bpm", "70", "--key", "A minor"]);
    assert_eq!((c, v["result"]["cached"].as_bool(), v["result"]["secs"].as_f64()), (0, Some(false), Some(12.0)), "{v} {err}");
    let key = v["result"]["key"].as_str().unwrap().to_string();
    assert!(d.join("takes/music").join(format!("{key}.wav")).is_file() && d.join("takes/music/manifest.json").is_file());
    let a = acalls();
    assert!(a.contains("gen calm maritime intro -i -d 12 --seed 1 --timesig 4 --bpm 70 --key A minor -f wav24 -o ") && a.lines().count() == 1, "{a}");
    assert!(err.contains("erzeugt") && err.contains(&key), "{err}");
    let (c, v, _) = go(&["music", "gen", "l", "--prompt", "calm maritime intro", "--dur", "12", "--bpm", "70", "--key", "A minor"]);
    assert_eq!((c, v["result"]["cached"].as_bool(), acalls().lines().count()), (0, Some(true), 1), "cache hit: {v}");
    assert_eq!(go(&["music", "gen", "l", "--prompt", "x", "--dur", "5"]).0, 1, "below acestep minimum");
    assert_eq!(go(&["music", "gen", "l", "--prompt", "x"]).0, 2, "--dur required");
    let (c, v, _) = go(&["music", "ls", "l"]);
    assert_eq!((c, v["result"]["music"].as_array().unwrap().len(), v["result"]["music"][0]["key"].as_str()), (0, 1, Some(key.as_str())));
    // failing acestep: error, no file, no manifest entry
    let o = cmd(&["music", "gen", "l", "--prompt", "boom", "--dur", "10"]).env("FAKE_FAIL", "1").output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!((std::fs::read_dir(d.join("takes/music")).unwrap().count(), acalls().lines().count()), (2, 2), "key.wav + manifest only");
    // cue add --gen / import + cue add --file / ls / rm
    let (c, v, _) = go(&["cue", "add", "l", "--gen", &key, "--at-line", "1", "--gain-db", "-20", "--fade-in", "2", "--duck"]);
    assert_eq!((c, v["result"]["cues"].as_array().unwrap().len(), v["result"]["cues"][0]["duck"].as_bool(), v["result"]["errors"].as_u64()), (0, 1, Some(true), Some(0)), "{v}");
    let pt = std::fs::read_to_string(d.join("project.toml")).unwrap();
    assert!(pt.contains("[[cues]]\nat_line = 1\nkind = \"music\"") && pt.contains("[cues.gen]\nprompt = \"calm maritime intro\"\ndur = 12\nseed = 1\nbpm = 70\nkey = \"A minor\"") && pt.contains("[speakers.Mara]"), "{pt}");
    assert!(go(&["cue", "add", "l", "--gen", "0000000000000000", "--at-line", "1"]).1["error"].as_str().unwrap().contains("nicht im Cache"));
    assert_eq!(go(&["cue", "add", "l", "--at-line", "1"]).0, 1, "no source");
    assert_eq!(go(&["cue", "add", "l", "--gen", &key]).0, 1, "no position");
    let (c, v, _) = go(&["cue", "add", "l", "--gen", &key, "--at-line", "99"]);
    assert!(c == 1 && v["error"].as_str().unwrap().contains("at_line 99") && v["error"].as_str().unwrap().contains("nichts geändert"), "{v}");
    let src = h.join("Meer (CC0).wav");
    std::fs::write(&src, wav(24_000 * 6)).unwrap();
    let (c, v, err) = go(&["cue", "import", "l", src.to_str().unwrap(), "--kind", "ambience", "--source", "https://freesound.org/x", "--license", "CC0"]);
    assert_eq!((c, v["result"]["file"].as_str(), v["result"]["secs"].as_f64()), (0, Some("assets/ambience/Meer-CC0.wav"), Some(6.0)), "{v}");
    assert!(d.join("assets/ambience/Meer-CC0.wav").is_file() && std::fs::read_to_string(d.join("assets/SOURCES.md")).unwrap().contains("CC0") && err.contains("cue add l --file assets/ambience/Meer-CC0.wav"));
    assert_eq!(go(&["cue", "import", "l", src.to_str().unwrap()]).0, 1, "exists");
    let (c, v, _) = go(&["cue", "add", "l", "--file", "assets/ambience/Meer-CC0.wav", "--kind", "ambience", "--at-line", "3", "--loop", "--gain-db", "-24", "--fade-out", "1.5"]);
    assert_eq!((c, v["result"]["cues"][1]["loop"].as_bool(), v["result"]["cues"][1]["at_line"].as_u64()), (0, Some(true), Some(3)), "{v}");
    let (c, v, _) = go(&["cue", "add", "l", "--file", "assets/ambience/Meer-CC0.wav", "--kind", "sfx", "--at-sec", "2.5", "--loop"]);
    assert!(c == 1 && v["error"].as_str().unwrap().contains("sfx"), "{v}");
    let (c, v, _) = go(&["cue", "ls", "l"]);
    assert_eq!((c, v["result"]["cues"].as_array().unwrap().len(), v["result"]["errors"].as_u64()), (0, 2, Some(0)));
    // export: both cues in the mix, music from the cache (0 acestep calls), speech via fake TTS
    let (c, v, err) = go(&["export", "l", "wav", "--no-disclosure"]);
    let r = &v["result"];
    assert_eq!((c, r["acestep_calls"].as_u64(), r["cues"].as_array().unwrap().len(), acalls().lines().count()), (0, Some(0), 2, 2), "{v} {err}");
    assert_eq!((r["cues"][0]["start"].as_f64(), r["cues"][0]["duck"].as_bool(), r["cues"][1]["clamped"].as_bool()), (Some(0.0), Some(true), Some(false)));
    assert!(r["cues"][1]["end"].as_f64().unwrap() > r["cues"][1]["start"].as_f64().unwrap());
    // missing cue file: lint + export refuse (exit 1), nothing rendered twice
    std::fs::remove_file(d.join("assets/ambience/Meer-CC0.wav")).unwrap();
    let (c, v, _) = go(&["lint", "l"]);
    assert!(c == 1 && v["result"]["issues"].as_array().unwrap().iter().any(|i| i["code"] == "cue_src"), "{v}");
    let (c, v, _) = go(&["export", "l", "wav"]);
    assert!(c == 1 && v["error"].as_str().unwrap().contains("Cue 2") && v["error"].as_str().unwrap().contains("cue ls"), "{v}");
    let (c, v, _) = go(&["cue", "rm", "l", "2"]);
    assert_eq!((c, v["result"]["cues"].as_array().unwrap().len()), (0, 1));
    assert_eq!(go(&["cue", "rm", "l", "5"]).0, 1);
    assert_eq!(go(&["lint", "l"]).0, 0);
    // cancel: acestep "runs" (sleeps, pid in ace.pid) → Ctrl-C ends the job within 2 s, child gone, no cache file
    let mut ch = cmd(&["music", "gen", "l", "--prompt", "slow", "--dur", "10", "--seed", "7"]).env("FAKE_SLEEP", "61.37").stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().unwrap();
    let t0 = std::time::Instant::now();
    let pid_file = h.join("ace.pid");
    let child_pid = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file).ok().and_then(|s| s.trim().parse::<i32>().ok()) {
            break pid;
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(5), "fake acestep did not start");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    // SAFETY: signal 0 only checks that the process exists
    let alive = |pid: i32| unsafe { kill(pid, 0) } == 0;
    assert!(alive(child_pid), "fake acestep (pid {child_pid}) not running");
    let t1 = std::time::Instant::now();
    assert_eq!(unsafe { kill(ch.id() as i32, 2) }, 0);
    let o = ch.wait_with_output().unwrap();
    let el = t1.elapsed();
    assert!(el < std::time::Duration::from_secs(2), "cancel took {el:?}");
    let v: Value = serde_json::from_slice(&o.stdout).unwrap_or(Value::Null);
    assert_eq!(o.status.code(), Some(1));
    assert!(v["error"].as_str().unwrap_or_default().contains("abgebrochen"), "{v}");
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert!(!alive(child_pid), "child {child_pid} still alive");
    assert_eq!(std::fs::read_dir(d.join("takes/music")).unwrap().count(), 2, "no file for the cancelled key, no temp");
    assert!(!h.join("Library/Application Support/funkhaus").exists() || std::fs::read_dir(h.join("Library/Application Support/funkhaus")).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".gpu.prio.")), "prio file removed");
}
