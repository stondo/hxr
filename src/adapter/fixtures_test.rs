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

    // --- hermes: state.db (current) ∪ session_*.json (legacy), db wins
    let he_db = home.join(".hermes/state.db");
    write(&he_db, "");
    {
        let c = rusqlite::Connection::open(&he_db).unwrap();
        c.execute_batch(
            "CREATE TABLE sessions (id TEXT, title TEXT, cwd TEXT, model TEXT,
             git_branch TEXT, message_count INTEGER, input_tokens INTEGER,
             output_tokens INTEGER, reasoning_tokens INTEGER,
             estimated_cost_usd REAL, parent_session_id TEXT,
             started_at REAL, ended_at REAL, last_activity_at REAL,
             archived INTEGER DEFAULT 0, hidden INTEGER DEFAULT 0,
             auto_archived INTEGER DEFAULT 0);
             CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT,
             role TEXT, content TEXT, timestamp REAL);
             INSERT INTO sessions VALUES ('20260101_120000_abcd01','Hermes One','/tmp/he1','deep-v4','main',
               3, 1200, 340, 56, 0.0123, NULL, 1767265200.5, 1767265600.5, 1767265610.5, 0, 0, 0);
             INSERT INTO sessions VALUES ('20260101_130000_child01','compression child',NULL,NULL,NULL,
               NULL, NULL, NULL, NULL, NULL, '20260101_120000_abcd01',
               1767265800.0, 1767265900.0, 1767265910.0, 0, 0, 0);
             INSERT INTO sessions VALUES ('20260101_140000_arch01','archived row',NULL,NULL,NULL,
               NULL, NULL, NULL, NULL, NULL, NULL,
               1767265900.0, NULL, NULL, 1, 0, 0);
             INSERT INTO sessions VALUES ('20260101_150000_dup0001','db version wins',NULL,NULL,NULL,
               NULL, NULL, NULL, NULL, NULL, NULL,
               1767265950.0, NULL, NULL, 0, 0, 0);
             INSERT INTO messages VALUES (1,'20260101_120000_abcd01','user','needleHE in state db',1.0);
             INSERT INTO messages VALUES (2,'20260101_120000_abcd01','assistant','hermes rendered reply',2.0);
             INSERT INTO messages VALUES (3,'20260101_150000_dup0001','user','dup body',1.0);",
        )
        .unwrap();
    }
    write(
        &home.join(".hermes/sessions/session_20260101_150000_dup0001.json"),
        "{\"session_id\":\"20260101_150000_dup0001\",\"model\":\"old-model\",
          \"messages\":[{\"role\":\"user\",\"content\":\"legacy-only body\"}]}",
    );
    write(
        &home.join(".hermes/sessions/session_20260102_090000_legac1.json"),
        "{\"session_id\":\"20260102_090000_legac1\",\"model\":\"old-model\",
          \"messages\":[{\"role\":\"assistant\",\"skip\":\"no\"},
           {\"role\":\"user\",\"content\":\"legacy hermes title\\nsecond line\"},
           {\"role\":\"assistant\",\"content\":\"needleHE in legacy file\"}]}",
    );
    write(&home.join(".hermes/sessions/sessions.json"), "{}");

    // --- aider: per-project markdown chat logs, sessions delimited by headers
    let ad_dir = home.join("lab-projects/adproj");
    write(
        &ad_dir.join(".aider.chat.history.md"),
        "# aider chat started at 2026-01-01 10:00:00\n\
         > /usr/bin/aider --message x\n\
         > Model: openai/DeepSeek-V4-Flash-0731 with whole edit format\n\
         #### first aider ask\n\
         thinking then reply\n\
         ```\nhello.py\n```\n\
         > Tokens: 100 sent, 50 received.\n\
         > Applied edit to hello.py\n\
         # aider chat started at 2026-01-02 11:00:00\n\
         > /usr/bin/aider --message y\n\
         > Model: openai/DeepSeek-V4-Flash-0731 with whole edit format\n\
         #### needleAD in aider block\n\
         second reply body\n\
         > Tokens: 200 sent, 80 received.\n",
    );
    write(&ad_dir.join(".aider.input.history"), "irrelevant\n");

    // --- gemini: tmp/<project>/chats jsonl + projects.json cwd map

    let gm_dir = home.join(".gemini/tmp/labproj/chats");
    write(
        &gm_dir.join("session-2026-01-01T10-00-00g11111.jsonl"),
        "{\"sessionId\":\"11111111-1111-4111-8111-111111111111\",\"projectHash\":\"h1\",\
          \"startTime\":\"2026-01-01T10:00:00Z\",\
          \"lastUpdated\":\"2026-01-01T10:02:00Z\",\"kind\":\"main\"}\n\
         {\"id\":\"e1\",\"timestamp\":\"2026-01-01T10:00:01Z\",\"type\":\"user\",\
          \"content\":[{\"text\":\"<session_context>\\nboilerplate\"}]}\n\
         {\"id\":\"e2\",\"timestamp\":\"2026-01-01T10:00:02Z\",\"type\":\"user\",\
          \"content\":[{\"text\":\"needleGM in gemini body\"}]}\n\
         {\"id\":\"e3\",\"timestamp\":\"2026-01-01T10:01:00Z\",\"type\":\"gemini\",\
          \"content\":\"gemini rendered reply\"}\n\
         {\"$set\":{\"lastUpdated\":\"2026-01-01T10:02:00Z\"}}\n",
    );
    // aborted session: user prompt recorded, model reply never landed
    write(
        &gm_dir.join("session-2026-01-02T11-00-00g22222.jsonl"),
        "{\"sessionId\":\"22222222-2222-4222-8222-222222222222\",\"projectHash\":\"h1\",\
          \"startTime\":\"2026-01-02T11:00:00Z\",\"lastUpdated\":\"2026-01-02T11:00:05Z\",\"kind\":\"main\"}\n\
         {\"id\":\"e4\",\"timestamp\":\"2026-01-02T11:00:01Z\",\"type\":\"user\",\
          \"content\":[{\"text\":\"<session_context>x\"}]}\n\
         {\"id\":\"e5\",\"timestamp\":\"2026-01-02T11:00:02Z\",\"type\":\"user\",\
          \"content\":[{\"text\":\"aborted but listed\"}]}\n",
    );
    // project name without a projects.json mapping → home fallback
    write(
        &home.join(".gemini/tmp/orphanproj/chats/session-2026-01-03T12-00-00g33333.jsonl"),
        "{\"sessionId\":\"33333333-3333-4333-8333-333333333333\",\"projectHash\":\"h2\",\
          \"startTime\":\"2026-01-03T12:00:00Z\",\"kind\":\"main\"}\n\
         {\"id\":\"e6\",\"timestamp\":\"2026-01-03T12:00:01Z\",\"type\":\"user\",\
          \"content\":[{\"text\":\"orphan project prompt\"}]}\n",
    );
    write(
        &home.join(".gemini/projects.json"),
        "{\"projects\":{\"/tmp/labproj\":\"labproj\"}}",
    );

    // --- crush: projects.json index + per-project crush.db
    let cs_dir = home.join("lab-projects/csproj/.crush");
    write(&cs_dir.join("crush.db"), "");
    {
        let c = rusqlite::Connection::open(cs_dir.join("crush.db")).unwrap();
        c.execute_batch(
            "CREATE TABLE sessions (id TEXT, title TEXT, parent_session_id TEXT,
             message_count INTEGER, prompt_tokens INTEGER, completion_tokens INTEGER,
             cost REAL, updated_at INTEGER);
             CREATE TABLE messages (id TEXT, session_id TEXT, role TEXT, parts TEXT,
             model TEXT, created_at INTEGER, updated_at INTEGER);
             INSERT INTO sessions VALUES ('c1111111-1111-4111-8111-111111111111',
               'Titled crush chat', NULL, 3, 500, 120, 0.045, 1767265800);
             INSERT INTO sessions VALUES ('c2222222-2222-4222-8222-222222222222',
               'Untitled Session', 'c1111111-1111-4111-8111-111111111111', 2, 10, 5, 0.0, 1767265900);
             INSERT INTO sessions VALUES ('c3333333-3333-4333-8333-333333333333',
               'Untitled Session', NULL, 1, 20, 8, 0.0, 1767265700);
             INSERT INTO messages VALUES ('m1','c1111111-1111-4111-8111-111111111111','user',
               '[{\"type\":\"text\",\"data\":{\"text\":\"needleCR in crush body\"}},{\"type\":\"finish\",\"data\":{\"reason\":\"stop\"}}]',NULL,1,1);
             INSERT INTO messages VALUES ('m2','c1111111-1111-4111-8111-111111111111','assistant',
               '[{\"type\":\"text\",\"data\":{\"text\":\"crush rendered reply\"}}]','DeepSeek-V4-Flash-0731',2,2);
             INSERT INTO messages VALUES ('m3','c3333333-3333-4333-8333-333333333333','user',
               '[{\"type\":\"text\",\"data\":{\"text\":\"untitled falls back to first ask\"}}]',NULL,1,1);",
        )
        .unwrap();
    }
    write(
        &home.join(".local/share/crush/projects.json"),
        &format!(
            "{{\"projects\":[{{\"path\":\"{}\",\"data_dir\":\"{}\",\"last_accessed\":\"2026-01-01T00:00:00Z\"}}]}}",
            home.join("lab-projects/csproj")
                .to_string_lossy()
                .replace('\\', "/"),
            cs_dir.to_string_lossy().replace('\\', "/")
        ),
    );

    // --- goose: sessions.db with parts-array transcripts

    let gs_db = home.join(".local/share/goose/sessions/sessions.db");
    write(&gs_db, "");
    {
        let c = rusqlite::Connection::open(&gs_db).unwrap();
        c.execute_batch(
            "CREATE TABLE sessions (id TEXT, name TEXT, description TEXT,
             user_set_name TEXT, session_type TEXT, working_dir TEXT,
             created_at TIMESTAMP, updated_at TIMESTAMP, extension_data TEXT,
             total_tokens INTEGER, input_tokens INTEGER, output_tokens INTEGER);
             CREATE TABLE messages (id INTEGER PRIMARY KEY, message_id TEXT,
             session_id TEXT, role TEXT, content_json TEXT,
             created_timestamp INTEGER, timestamp TIMESTAMP, tokens INTEGER,
             metadata_json TEXT);
             INSERT INTO sessions VALUES ('20260101_1','CLI Session','Summarize the repo','', 'user','/tmp/gs1',
               '2026-01-01 10:00:00','2026-01-01 10:05:00','{}', 4200, 3900, 300);
             INSERT INTO sessions VALUES ('20260101_2','My custom title','',1,'user','/tmp/gs2',
               '2026-01-01 11:00:00','2026-01-01 11:01:00','{}', NULL, NULL, NULL);
             INSERT INTO messages VALUES (1,'m1','20260101_1','user',
               '[{\"type\":\"text\",\"text\":\"needleGS in goose body\"}]', 1767265200, '2026-01-01 10:00:00', 5, '{}');
             INSERT INTO messages VALUES (2,'m2','20260101_1','user',
               '[{\"type\":\"text\",\"text\":\"<turn-context>\\nboilerplate</turn-context>\"}]', 1767265200, '2026-01-01 10:00:00', 5, '{}');
             INSERT INTO messages VALUES (3,'m3','20260101_1','assistant',
               '[{\"type\":\"text\",\"text\":\"goose rendered reply\"}]', 1767265260, '2026-01-01 10:01:00', 5, '{}');
             INSERT INTO messages VALUES (4,'m4','20260101_2','user',
               '[{\"type\":\"text\",\"text\":\"no-dir-title fallback\"}]', 1767265800, '2026-01-01 11:00:00', 5, '{}');",
        )
        .unwrap();
    }

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

        let he = run_adapter("hermes");
        assert_eq!(
            he.len(),
            3,
            "child + archived rows filtered, legacy dup deduped against the db"
        );
        let hd = find(&he, "20260101_120000_abcd01");
        assert_eq!(hd.title, "Hermes One");
        assert_eq!(hd.cwd, Path::new("/tmp/he1"));
        assert_eq!(hd.updated_ms, 1_767_265_610_500, "last_activity_at wins");
        assert_eq!(hd.extra.get("model").map(String::as_str), Some("deep-v4"));
        assert_eq!(hd.extra.get("branch").map(String::as_str), Some("main"));
        assert_eq!(
            hd.extra.get("tokens").map(String::as_str),
            Some("in 1200 / out 340 / reason 56")
        );
        assert_eq!(
            hd.extra.get("totalCostUsd").map(String::as_str),
            Some("0.0123")
        );
        let hl = find(&he, "20260102_090000_legac1");
        assert_eq!(hl.title, "legacy hermes title", "first user message");
        assert_eq!(hl.cwd, home, "legacy files record no cwd: home fallback");
        assert_eq!(
            find(&he, "20260101_150000_dup0001").title,
            "db version wins"
        );
        let (argv, cwd) = registry(false)
            .into_iter()
            .find(|h| h.name() == "hermes")
            .unwrap()
            .resume(hd, &[]);
        assert_eq!(argv, vec!["hermes", "--resume", "20260101_120000_abcd01"]);
        assert_eq!(cwd, Path::new("/tmp/he1"));

        let gs = run_adapter("goose");
        assert_eq!(gs.len(), 2);
        let g1 = find(&gs, "20260101_1");
        assert_eq!(g1.title, "Summarize the repo", "description beats name");
        assert_eq!(g1.cwd, Path::new("/tmp/gs1"));
        assert_eq!(g1.updated_ms, 1_767_261_900_000, "UTC updated_at → ms");
        assert_eq!(
            g1.extra.get("tokens").map(String::as_str),
            Some("4200"),
            "total_tokens preferred"
        );
        assert_eq!(
            find(&gs, "20260101_2").title,
            "My custom title",
            "user-renamed name wins"
        );
        let (argv, cwd) = registry(false)
            .into_iter()
            .find(|h| h.name() == "goose")
            .unwrap()
            .resume(g1, &[]);
        assert_eq!(
            argv,
            vec!["goose", "session", "--resume", "--session-id", "20260101_1"]
        );
        assert_eq!(cwd, Path::new("/tmp/gs1"));

        let gm = run_adapter("gemini");
        assert_eq!(gm.len(), 3, "aborted sessions stay listed");
        let g1111 = find(&gm, "11111111-1111-4111-8111-111111111111");
        assert_eq!(
            g1111.title, "needleGM in gemini body",
            "session_context boilerplate skipped"
        );
        assert_eq!(g1111.cwd, Path::new("/tmp/labproj"), "projects.json map");
        assert_eq!(
            find(&gm, "22222222-2222-4222-8222-222222222222").title,
            "aborted but listed"
        );
        assert_eq!(
            find(&gm, "33333333-3333-4333-8333-333333333333").cwd,
            home,
            "unmapped project name falls back to home"
        );
        let (argv, cwd) = registry(false)
            .into_iter()
            .find(|h| h.name() == "gemini")
            .unwrap()
            .resume(g1111, &[]);
        assert_eq!(argv[0], "gemini");
        assert!(argv.ends_with(&["--session-file".to_string(), g1111.extra["path"].clone()]));
        assert_eq!(cwd, Path::new("/tmp/labproj"));

        let ad = run_adapter("aider");
        assert_eq!(ad.len(), 2, "one session per started-block");
        // ids embed native-separator paths (opaque) — look up by block suffix
        let a0 = ad.iter().find(|s| s.id.ends_with("#0")).unwrap();
        assert_eq!(a0.title, "first aider ask");
        assert_eq!(a0.cwd, home.join("lab-projects/adproj"));
        assert_eq!(
            a0.extra.get("model").map(String::as_str),
            Some("openai/DeepSeek-V4-Flash-0731")
        );
        assert_eq!(a0.extra.get("tokens").map(String::as_str), Some("100 + 50"));
        let a1 = ad.iter().find(|s| s.id.ends_with("#1")).unwrap();
        assert_eq!(a1.title, "needleAD in aider block");
        let (argv, cwd) = registry(false)
            .into_iter()
            .find(|h| h.name() == "aider")
            .unwrap()
            .resume(a0, &[]);
        assert_eq!(argv, vec!["aider", "--restore-chat-history"]);
        assert_eq!(cwd, home.join("lab-projects/adproj"));

        let cr = run_adapter("crush");
        assert_eq!(cr.len(), 2, "child session hidden without --all");
        let c1 = find(&cr, "c1111111-1111-4111-8111-111111111111");
        assert_eq!(c1.title, "Titled crush chat");
        assert_eq!(c1.cwd, home.join("lab-projects/csproj"));
        assert_eq!(c1.updated_ms, 1_767_265_800_000, "epoch seconds → ms");
        assert_eq!(
            c1.extra.get("tokens").map(String::as_str),
            Some("in 500 / out 120")
        );
        assert_eq!(
            c1.extra.get("totalCostUsd").map(String::as_str),
            Some("0.0450")
        );
        assert_eq!(
            find(&cr, "c3333333-3333-4333-8333-333333333333").title,
            "untitled falls back to first ask"
        );
        let (argv, cwd) = registry(false)
            .into_iter()
            .find(|h| h.name() == "crush")
            .unwrap()
            .resume(c1, &[]);
        assert_eq!(
            argv,
            vec!["crush", "-s", "c1111111-1111-4111-8111-111111111111"]
        );
        assert_eq!(cwd, home.join("lab-projects/csproj"));

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
        assert!(cm("hermes", hd, "needleHE").is_some(), "db body search");
        assert!(
            cm("hermes", hl, "needleHE").is_some(),
            "legacy file body search"
        );
        assert!(
            rd("hermes", hd).unwrap().contains("hermes rendered reply"),
            "db transcript"
        );
        assert!(
            rd("hermes", hl)
                .unwrap()
                .contains("needleHE in legacy file"),
            "legacy transcript"
        );
        assert!(cm("goose", g1, "needleGS").is_some(), "goose body search");
        assert!(
            rd("goose", g1).unwrap().contains("goose rendered reply")
                && !rd("goose", g1).unwrap().contains("turn-context"),
            "goose transcript skips boilerplate"
        );
        assert!(
            cm("gemini", g1111, "needleGM").is_some(),
            "gemini body search"
        );
        assert!(
            rd("gemini", g1111)
                .unwrap()
                .contains("gemini rendered reply"),
            "gemini transcript"
        );
        assert!(cm("aider", a1, "needleAD").is_some(), "aider block search");
        assert!(
            rd("aider", a0).unwrap().contains("[user]\nfirst aider ask"),
            "aider transcript user turn"
        );
        assert!(
            rd("aider", a0).unwrap().contains("hello.py"),
            "aider transcript keeps file listing"
        );
        assert!(cm("crush", c1, "needleCR").is_some(), "crush body search");
        assert!(
            rd("crush", c1).unwrap().contains("crush rendered reply"),
            "crush transcript text parts"
        );
        assert!(
            !rd("crush", c1).unwrap().contains("finish"),
            "finish parts carry no prose"
        );

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
