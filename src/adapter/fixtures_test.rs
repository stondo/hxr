//! Fixture-based adapter tests (drift protection). Builds a synthetic
//! `$HXR_HOME` containing one minimal store per harness — including the
//! sqlite schemas we depend on — and asserts discover()/resume() output.
//! When an upstream CLI changes its storage format, the corresponding
//! assertion here is what fails first.

use super::registry;
use crate::model::Session;
use std::path::{Path, PathBuf};

fn setup_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("opencode")
        .join(format!("hxr-fixtures-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, content: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

fn with_hxr_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
    // Safety: ENV_LOCK serializes this against env-reading tests; fixture
    // tests run one at a time.
    let _guard = super::util::ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var("HXR_HOME", home);
    }
    let out = f();
    unsafe {
        std::env::remove_var("HXR_HOME");
    }
    out
}

fn find<'a>(sessions: &'a [Session], id: &str) -> &'a Session {
    sessions
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("session {id} not found"))
}

fn run_adapter(name: &str) -> Vec<Session> {
    registry(false)
        .into_iter()
        .find(|h| h.name() == name)
        .unwrap_or_else(|| panic!("no adapter {name}"))
        .discover()
        .unwrap_or_else(|e| panic!("{name} discover failed: {e:#}"))
}

#[test]
fn all_adapters_read_fixtures() {
    let home = setup_home("all");

    // --- opencode: session_v2 with title + slug fallback, archived filtered
    let oc_db = home.join(".local/share/opencode/opencode.db");
    write(&oc_db, "");
    {
        let c = rusqlite::Connection::open(&oc_db).unwrap();
        c.execute_batch(
            "CREATE TABLE session_v2 (id TEXT, directory TEXT, title TEXT, slug TEXT,
             time_updated INTEGER, parent_id TEXT, time_archived INTEGER);
             INSERT INTO session_v2 VALUES ('ses_AAA','/tmp/oc','Title A','slug-a',2000,NULL,NULL);
             INSERT INTO session_v2 VALUES ('ses_BBB','/tmp/oc','','slug-b',1000,NULL,NULL);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL,
             session_id TEXT NOT NULL, time_created INTEGER NOT NULL,
             time_updated INTEGER NOT NULL, data TEXT NOT NULL);
             INSERT INTO session_v2 VALUES ('ses_ARCH','/tmp/oc','x','x',3000,NULL,99);
             INSERT INTO part VALUES ('p1','m1','ses_AAA',1,1,'{\"type\":\"text\",\"text\":\"the needleOC hides here\"}');
             INSERT INTO part VALUES ('p2','m1','ses_BBB',1,1,'{\"type\":\"text\",\"text\":\"nothing relevant\"}');",
        )
        .unwrap();
    }

    // --- claude: transcript with ai-title + cwd, plus history.jsonl fallback
    let cl_dir = home.join(".claude/projects/-tmp-cl");
    write(
        &cl_dir.join("11111111-1111-1111-1111-111111111111.jsonl"),
        "{\"type\":\"user\",\"cwd\":\"/tmp/cl\",\"message\":{\"role\":\"user\",\"content\":\"first ask\"}}\n\
         {\"type\":\"ai-title\",\"aiTitle\":\"AI Title\"}\n\
         {\"type\":\"user\",\"cwd\":\"/tmp/cl\",\"message\":{\"role\":\"user\",\"content\":\"deep needleCL line\"}}\n",
    );
    write(
        &cl_dir.join("22222222-2222-2222-2222-222222222222.jsonl"),
        "{\"type\":\"user\",\"cwd\":\"/tmp/cl\",\"message\":{\"role\":\"user\",\"content\":\"<command-caveat>skip\"}}\n",
    );
    write(
        &home.join(".claude/history.jsonl"),
        "{\"display\":\"/slash\",\"project\":\"/tmp/cl\",\"sessionId\":\"22222222-2222-2222-2222-222222222222\",\"timestamp\":1}\n\
         {\"display\":\"typed prompt\",\"project\":\"/tmp/cl\",\"sessionId\":\"22222222-2222-2222-2222-222222222222\",\"timestamp\":2}\n",
    );

    // --- codex: state db with titled row + first_user_message fallback
    let cx_rollout = home.join("rollout-t1.jsonl");
    write(
        &cx_rollout,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\"}}\n\
         {\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"content\":[{\"type\":\"input_text\",\"text\":\"needleCX in rollout\"}]}}\n",
    );
    let cx_db = home.join(".codex/state_9.sqlite");
    write(&cx_db, "");
    {
        let c = rusqlite::Connection::open(&cx_db).unwrap();
        c.execute_batch(
            "CREATE TABLE threads (id TEXT, rollout_path TEXT, created_at INTEGER,
             updated_at INTEGER, cwd TEXT, title TEXT, first_user_message TEXT,
             updated_at_ms INTEGER, recency_at INTEGER, archived INTEGER,
             model TEXT, git_branch TEXT, tokens_used INTEGER);
             INSERT INTO threads VALUES ('t1','/r1',100,200,'/tmp/cx1','T One','hello world',2000,2000,0,'m1','main',4242);
             INSERT INTO threads VALUES ('t2','/r2',100,200,'/tmp/cx2','','fum title',1000,1000,0,NULL,NULL,NULL);",
        )
        .unwrap();
        c.execute(
            "UPDATE threads SET rollout_path = ?1 WHERE id = 't1'",
            [cx_rollout.to_string_lossy().into_owned()],
        )
        .unwrap();
    }

    // --- pi: session meta line + user message
    write(
        &home.join(".pi/agent/sessions/-tmp-pi/2026-01-01T00-00-00-000Z_pid1.jsonl"),
        "{\"type\":\"session\",\"version\":3,\"id\":\"pid1\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"cwd\":\"/tmp/pi\"}\n\
         {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"pi ask\"}]}}\n",
    );

    // --- omp: history.db with titles and prompt history
    let omp_db = home.join(".omp/agent/history.db");
    write(&omp_db, "");
    {
        let c = rusqlite::Connection::open(&omp_db).unwrap();
        c.execute_batch(
            "CREATE TABLE session_titles (session_id TEXT PRIMARY KEY, title TEXT NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE history (id INTEGER PRIMARY KEY AUTOINCREMENT, prompt TEXT NOT NULL, created_at INTEGER NOT NULL, cwd TEXT, session_id TEXT, use_count INTEGER NOT NULL);
             INSERT INTO session_titles VALUES ('omp1','OMP Title',500);
             INSERT INTO history (prompt, created_at, cwd, session_id, use_count) VALUES ('prompt one',400,'/tmp/omp-old','omp1',1);
             INSERT INTO history (prompt, created_at, cwd, session_id, use_count) VALUES ('prompt two',450,'/tmp/omp-new','omp1',1);\n             INSERT INTO history (prompt, created_at, cwd, session_id, use_count) VALUES ('needleOMP wording',460,'/tmp/omp-new','omp1',1);",
        )
        .unwrap();
    }

    // --- qwen: chats jsonl with parts-shaped message
    write(
        &home.join(".qwen/projects/-tmp-qw/chats/q1.jsonl"),
        "{\"sessionId\":\"q1\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"type\":\"user\",\"cwd\":\"/tmp/qw\",\"gitBranch\":\"main\",\"message\":{\"role\":\"user\",\"parts\":[{\"text\":\"qwen ask\"}]}}\n",
    );

    // --- copilot: workspace.yaml session dirs
    let cp_dir = home.join(".copilot/session-state/9d03122e-4b92-497b-b805-7b67332ae777");
    write(
        &cp_dir.join("workspace.yaml"),
        "id: 9d03122e-4b92-497b-b805-7b67332ae777\n\
         cwd: /tmp/cp\n\
         branch: main\n\
         created_at: 2026-05-07T22:11:44.339Z\n\
         updated_at: 2026-05-07T22:11:44.522Z\n\
         name: commit everything and push\n\
         summary: commit everything and push\n",
    );

    // --- omp main transcript sibling file (line 1 title, line 2 session meta)
    write(
        &home.join(".omp/agent/sessions/-tmp--/2026-01-01T00-00-00-000Z_omp1.jsonl"),
        "{\"type\":\"title\",\"title\":\"OMP fixture\"}\n\
         {\"type\":\"session\",\"version\":\"3\",\"id\":\"omp1\",\"cwd\":\"/tmp/omp\"}\n\
         {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"needleOMPmain ask\"}}\n",
    );

    // --- kimi: session index + wire event streams
    let km_dir = home.join(".kimi-code/sessions/wd_demo_ab12/session_k1");
    write(
        &km_dir.join("agents/main/wire.jsonl"),
        "{\"type\":\"metadata\",\"protocol_version\":\"1.5\",\"created_at\":100}\n\
         {\"type\":\"turn.prompt\",\"input\":[{\"type\":\"text\",\"text\":\"kimi ask\\nsecond line\"}]}\n\
         {\"type\":\"llm.request\",\"model\":\"k3\"}\n\
         {\"type\":\"turn.prompt\",\"input\": [{\"type\":\"text\",\"text\":\"later needleKI turn\"}]}\n\
         {\"type\":\"agent.message.appended\",\"message\":{\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"kimi rendered turn\"}]}}}\n\
         {\"type\":\"usage.record\",\"usage\":{\"inputOther\":100,\"output\":10,\"inputCacheRead\":50,\"inputCacheCreation\":0}}\n\
         {\"type\":\"usage.record\",\"usage\":{\"inputOther\":40,\"output\":20,\"inputCacheRead\":0,\"inputCacheCreation\":0}}\n",
    );
    write(
        &home.join(".kimi-code/session_index.jsonl"),
        &format!(
            "{{\"sessionId\":\"session_k1\",\"sessionDir\":\"{}\",\"workDir\":\"/tmp/kimi\"}}\n",
            km_dir.to_string_lossy().replace('\\', "/")
        ),
    );

    // --- reasonix: meta + transcript, archive filtered
    write(
        &home.join(".reasonix/sessions/demo.meta.json"),
        "{\"summary\":\"R summary\",\"workspace\":\"/tmp/rp\",\"branch\":\"main\",\"turnCount\":5,\"totalCostUsd\":0.5}",
    );
    write(
        &home.join(".reasonix/sessions/demo.jsonl"),
        "{\"role\":\"user\",\"content\":\"needleRX in transcript\"}\n\
         {\"role\":\"assistant\",\"content\":\"reasonix rendered reply\"}\n",
    );
    write(
        &home.join(".reasonix/sessions/gone__archive_1.meta.json"),
        "{\"summary\":\"archived\",\"workspace\":\"/tmp\"}",
    );

    // ---------------- assertions (HXR_HOME set for the whole block) --------
    with_hxr_home(&home, || {
        let oc = run_adapter("opencode");
        assert_eq!(oc.len(), 2, "archived row must be filtered");
        assert_eq!(find(&oc, "ses_AAA").title, "Title A");
        assert_eq!(
            find(&oc, "ses_BBB").title,
            "slug-b",
            "empty title falls back to slug"
        );

        let cl = run_adapter("claude");
        assert_eq!(
            find(&cl, "11111111-1111-1111-1111-111111111111").title,
            "AI Title"
        );
        assert_eq!(
            find(&cl, "11111111-1111-1111-1111-111111111111").cwd,
            Path::new("/tmp/cl")
        );
        assert_eq!(
            find(&cl, "22222222-2222-2222-2222-222222222222").title,
            "typed prompt",
            "history.jsonl display fills missing ai-title (slash commands skipped)"
        );

        let cx = run_adapter("codex");
        assert_eq!(find(&cx, "t1").title, "T One");
        assert_eq!(
            find(&cx, "t1").extra.get("model").map(String::as_str),
            Some("m1")
        );
        assert_eq!(
            find(&cx, "t2").title,
            "fum title",
            "empty title falls back to first_user_message"
        );
        assert_eq!(
            find(&cx, "t1").extra.get("tokens").map(String::as_str),
            Some("4242")
        );

        let pi = run_adapter("pi");
        let p = find(&pi, "pid1");
        assert_eq!(p.title, "pi ask");
        assert_eq!(p.cwd, Path::new("/tmp/pi"));
        let (argv, cwd) = registry(false)
            .into_iter()
            .find(|h| h.name() == "pi")
            .unwrap()
            .resume(p, &[]);
        assert_eq!(argv[0], "pi");
        assert!(
            argv[2].ends_with("pid1.jsonl"),
            "resume by full path: {argv:?}"
        );
        assert_eq!(cwd, Path::new("/tmp/pi"));

        let om = run_adapter("omp");
        let o = find(&om, "omp1");
        assert_eq!(o.title, "OMP Title");
        assert_eq!(o.cwd, Path::new("/tmp/omp-new"), "latest history cwd wins");
        assert_eq!(o.updated_ms, 500_000);

        let qw = run_adapter("qwen");
        let q = find(&qw, "q1");
        assert_eq!(q.title, "qwen ask");
        assert_eq!(q.extra.get("branch").map(String::as_str), Some("main"));

        let km = run_adapter("kimi");
        let k = find(&km, "session_k1");
        assert_eq!(k.title, "kimi ask");
        assert_eq!(k.cwd, Path::new("/tmp/kimi"));
        assert_eq!(k.extra.get("model").map(String::as_str), Some("k3"));

        let cp = run_adapter("copilot");
        let c = find(&cp, "9d03122e-4b92-497b-b805-7b67332ae777");
        assert_eq!(c.title, "commit everything and push");
        assert_eq!(c.cwd, Path::new("/tmp/cp"));
        assert!(c.updated_ms > 0);

        // --- render (hxr show): file-backed adapters produce turns
        let rd = |harness: &str, s: &Session| {
            registry(false)
                .into_iter()
                .find(|h| h.name() == harness)
                .unwrap()
                .render(s)
        };
        assert!(
            rd("claude", find(&cl, "11111111-1111-1111-1111-111111111111"))
                .unwrap()
                .contains("[user]")
        );
        assert!(
            rd("kimi", find(&km, "session_k1"))
                .unwrap()
                .contains("[user]")
        );

        // --- content search (-G): every adapter finds its needle
        let rx = run_adapter("reasonix");
        assert_eq!(rx.len(), 1, "__archive_ sessions must be filtered");
        let pat = |p: &str| crate::adapter::SearchPattern::new(p).unwrap();
        let cm = |harness: &str, s: &Session, p: &str| {
            registry(false)
                .into_iter()
                .find(|h| h.name() == harness)
                .unwrap()
                .content_match(s, &pat(p))
        };
        assert!(cm("opencode", find(&oc, "ses_AAA"), "needleOC").is_some());
        assert!(cm("opencode", find(&oc, "ses_BBB"), "needleOC").is_none());
        assert!(
            cm(
                "claude",
                find(&cl, "11111111-1111-1111-1111-111111111111"),
                "needleCL"
            )
            .is_some()
        );
        assert!(cm("codex", find(&cx, "t1"), "needleCX").is_some());
        assert!(cm("pi", find(&pi, "pid1"), "pi ask").is_some());
        assert!(cm("omp", find(&om, "omp1"), "needleOMPmain").is_some());
        assert!(
            cm("omp", find(&om, "omp1"), "needleOMP").is_some(),
            "prompts still searched"
        );
        assert!(rd("omp", find(&om, "omp1")).unwrap().contains("[user]"));
        assert!(
            rd("reasonix", find(&rx, "demo"))
                .unwrap()
                .contains("reasonix rendered reply")
        );
        let kstats = registry(false)
            .into_iter()
            .find(|h| h.name() == "kimi")
            .unwrap()
            .stats(find(&km, "session_k1"))
            .expect("kimi stats");
        assert_eq!(kstats.get("tokens").map(String::as_str), Some("220"));
        assert!(cm("qwen", find(&qw, "q1"), "qwen ask").is_some());
        assert!(cm("kimi", find(&km, "session_k1"), "needleKI").is_some());
        assert!(cm("reasonix", find(&rx, "demo"), "needleRX").is_some());

        let r = find(&rx, "demo");
        assert_eq!(r.title, "R summary");
        assert_eq!(r.cwd, Path::new("/tmp/rp"));
        assert_eq!(r.extra.get("turnCount").map(String::as_str), Some("5"));
        assert_eq!(
            r.extra.get("totalCostUsd").map(String::as_str),
            Some("0.5000")
        );
    });

    let _ = std::fs::remove_dir_all(&home);
}
