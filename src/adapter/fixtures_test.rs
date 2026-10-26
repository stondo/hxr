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
    registry()
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
             INSERT INTO session_v2 VALUES ('ses_ARCH','/tmp/oc','x','x',3000,NULL,99);",
        )
        .unwrap();
    }

    // --- claude: transcript with ai-title + cwd, plus history.jsonl fallback
    let cl_dir = home.join(".claude/projects/-tmp-cl");
    write(
        &cl_dir.join("11111111-1111-1111-1111-111111111111.jsonl"),
        "{\"type\":\"user\",\"cwd\":\"/tmp/cl\",\"message\":{\"role\":\"user\",\"content\":\"first ask\"}}\n\
         {\"type\":\"ai-title\",\"aiTitle\":\"AI Title\"}\n",
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
    let cx_db = home.join(".codex/state_9.sqlite");
    write(&cx_db, "");
    {
        let c = rusqlite::Connection::open(&cx_db).unwrap();
        c.execute_batch(
            "CREATE TABLE threads (id TEXT, rollout_path TEXT, created_at INTEGER,
             updated_at INTEGER, cwd TEXT, title TEXT, first_user_message TEXT,
             updated_at_ms INTEGER, recency_at INTEGER, archived INTEGER,
             model TEXT, git_branch TEXT);
             INSERT INTO threads VALUES ('t1','/r1',100,200,'/tmp/cx1','T One','hello world',2000,2000,0,'m1','main');
             INSERT INTO threads VALUES ('t2','/r2',100,200,'/tmp/cx2','','fum title',1000,1000,0,NULL,NULL);",
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
             INSERT INTO history (prompt, created_at, cwd, session_id, use_count) VALUES ('prompt two',450,'/tmp/omp-new','omp1',1);",
        )
        .unwrap();
    }

    // --- qwen: chats jsonl with parts-shaped message
    write(
        &home.join(".qwen/projects/-tmp-qw/chats/q1.jsonl"),
        "{\"sessionId\":\"q1\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"type\":\"user\",\"cwd\":\"/tmp/qw\",\"gitBranch\":\"main\",\"message\":{\"role\":\"user\",\"parts\":[{\"text\":\"qwen ask\"}]}}\n",
    );

    // --- kimi: session index + wire event streams
    let km_dir = home.join(".kimi-code/sessions/wd_demo_ab12/session_k1");
    write(
        &km_dir.join("agents/main/wire.jsonl"),
        "{\"type\":\"metadata\",\"protocol_version\":\"1.5\",\"created_at\":100}\n\
         {\"type\":\"turn.prompt\",\"input\":[{\"type\":\"text\",\"text\":\"kimi ask\\nsecond line\"}]}\n\
         {\"type\":\"llm.request\",\"model\":\"k3\"}\n",
    );
    write(
        &home.join(".kimi-code/session_index.jsonl"),
        &format!(
            "{{\"sessionId\":\"session_k1\",\"sessionDir\":\"{}\",\"workDir\":\"/tmp/kimi\"}}\n",
            km_dir.to_string_lossy()
        ),
    );

    // --- reasonix: meta + transcript, archive filtered
    write(
        &home.join(".reasonix/sessions/demo.meta.json"),
        "{\"summary\":\"R summary\",\"workspace\":\"/tmp/rp\",\"branch\":\"main\",\"turnCount\":5,\"totalCostUsd\":0.5}",
    );
    write(&home.join(".reasonix/sessions/demo.jsonl"), "{}\n");
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

        let pi = run_adapter("pi");
        let p = find(&pi, "pid1");
        assert_eq!(p.title, "pi ask");
        assert_eq!(p.cwd, Path::new("/tmp/pi"));
        let (argv, cwd) = registry()
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

        let rx = run_adapter("reasonix");
        assert_eq!(rx.len(), 1, "__archive_ sessions must be filtered");
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
