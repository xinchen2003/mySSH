//! core-store 集成测试：真实 SQLite 临时库，CRUD/级联/审计游标/DPAPI 凭据回环。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use core_store::{Actor, AuthType, CredentialKind, Secret, SessionRecord, Store};

#[tokio::test]
async fn settings_kv_roundtrip() {
    let path = temp_db("settings");
    let _ = std::fs::remove_file(&path);
    let store = Store::open(&path).await.unwrap();
    let s = store.settings();
    assert!(s.get("theme").await.unwrap().is_none());
    s.set("theme", "\"one-dark\"").await.unwrap();
    assert_eq!(
        s.get("theme").await.unwrap().as_deref(),
        Some("\"one-dark\"")
    );
    s.set("theme", "\"nord\"").await.unwrap(); // upsert 覆盖
    s.set("font.size", "14").await.unwrap();
    let all = s.all().await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].0, "font.size"); // ORDER BY key
    s.delete("theme").await.unwrap();
    assert!(s.get("theme").await.unwrap().is_none());
}

fn temp_db(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("myssh-store-test-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("myssh.db")
}

fn sample(id: &str, name: &str) -> SessionRecord {
    SessionRecord {
        id: id.into(),
        name: name.into(),
        host: "192.0.2.10".into(),
        port: 22,
        user: "ops".into(),
        auth_type: AuthType::Password,
        key_path: None,
        group_path: "生产/华东".into(),
        tags: vec!["prod".into(), "web".into()],
        jump_chain: vec![],
        command: None,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

#[tokio::test]
async fn session_crud_and_list() {
    let path = temp_db("crud");
    let store = Store::open(&path).await.expect("open");

    let rec = store
        .sessions()
        .upsert(&sample("s1", "生产 Web"))
        .await
        .expect("insert");
    assert_eq!(rec.name, "生产 Web");
    assert!(!rec.created_at.is_empty(), "created_at 由 DB 默认填充");

    // upsert 同 id = 更新
    let mut changed = sample("s1", "生产 Web-02");
    changed.port = 2222;
    let rec = store.sessions().upsert(&changed).await.expect("update");
    assert_eq!(rec.name, "生产 Web-02");
    assert_eq!(rec.port, 2222);

    store
        .sessions()
        .upsert(&sample("s2", "跳板机"))
        .await
        .expect("insert 2");
    let all = store.sessions().list().await.expect("list");
    assert_eq!(all.len(), 2);

    store.sessions().delete("s1").await.expect("delete");
    assert!(matches!(
        store.sessions().get("s1").await,
        Err(core_store::StoreError::NotFound(_))
    ));
    let all = store.sessions().list().await.expect("list");
    assert_eq!(all.len(), 1);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn credential_roundtrip_and_cascade() {
    let path = temp_db("cred");
    let store = Store::open(&path).await.expect("open");
    store
        .sessions()
        .upsert(&sample("s1", "带凭据"))
        .await
        .expect("insert session");

    let creds = store.credentials();
    creds
        .put(
            "s1",
            CredentialKind::Password,
            &Secret::new(b"correct horse".to_vec()),
        )
        .await
        .expect("put");
    let got = creds.get("s1").await.expect("get");
    assert_eq!(got.expose(), b"correct horse");
    // Debug 不含明文
    assert!(!format!("{got:?}").contains("correct"));

    // 覆盖写
    creds
        .put(
            "s1",
            CredentialKind::Password,
            &Secret::new(b"new secret".to_vec()),
        )
        .await
        .expect("overwrite");
    assert_eq!(creds.get("s1").await.unwrap().expose(), b"new secret");

    // 会话删除级联清凭据
    store.sessions().delete("s1").await.expect("delete");
    assert!(matches!(
        creds.get("s1").await,
        Err(core_store::StoreError::NotFound(_))
    ));

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn audit_append_and_cursor_pagination() {
    let path = temp_db("audit");
    let store = Store::open(&path).await.expect("open");
    let audit = store.audit();

    for i in 0..5 {
        audit
            .append(
                Actor::Gui,
                Some("s1"),
                "term_input",
                &serde_json::json!({ "i": i }),
            )
            .await
            .expect("append");
    }
    let (page1, cursor) = audit.query(None, 2).await.expect("page1");
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0].detail["i"], 4, "倒序最新在前");
    let cursor = cursor.expect("应有下一页");

    // id 序 1..5 对应 i 序 0..4；page1 取 i4,i3（游标 id3）→ page2 取 i1,i0 即见底
    let (page2, cursor2) = audit.query(Some(cursor), 2).await.expect("page2");
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[0].detail["i"], 1);
    assert_eq!(page2[1].detail["i"], 0);
    assert!(cursor2.is_none(), "已见底无游标");

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn import_openssh_config() {
    let path = temp_db("import");
    let store = Store::open(&path).await.expect("open");
    let text = r#"
# 注释行
Host prod-web prod-web.internal
    HostName 192.0.2.10
    User ops
    Port 2222
    IdentityFile ~/.ssh/id_ed25519

Host *.internal
    User jumped

Host nouser
    HostName 192.0.2.20

Host legacy
    HostName 192.0.2.30
    User root
"#;
    let outcome = core_store::import_openssh(&store, text, "C:\\Users\\me")
        .await
        .expect("import");
    assert_eq!(outcome.imported, 2, "prod-web + legacy 可导入");
    assert_eq!(outcome.skipped, 2, "通配块 + 无 user 块");

    let prod = store.sessions().get("ssh-prod-web").await.expect("get");
    assert_eq!(prod.host, "192.0.2.10");
    assert_eq!(prod.port, 2222);
    assert_eq!(prod.user, "ops");
    assert_eq!(prod.auth_type, AuthType::PublicKey);
    assert_eq!(
        prod.key_path.as_deref(),
        Some("C:\\Users\\me/.ssh/id_ed25519")
    );

    let legacy = store.sessions().get("ssh-legacy").await.expect("get");
    assert_eq!(legacy.auth_type, AuthType::Agent, "无 IdentityFile → agent");

    // 幂等：重复导入不增生
    let again = core_store::import_openssh(&store, text, "C:\\Users\\me")
        .await
        .expect("reimport");
    assert_eq!(again.imported, 2);
    assert_eq!(store.sessions().list().await.unwrap().len(), 2);

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn tunnel_defs_crud_and_jump_chain() {
    let path = temp_db("tunneldefs");
    let store = Store::open(&path).await.expect("open");
    store
        .sessions()
        .upsert(&sample("s1", "目标机"))
        .await
        .expect("session");

    // 迁移 0002：jump_chain 往返
    let mut hop = sample("s1", "目标机");
    hop.jump_chain = vec!["jump-a".into(), "jump-b".into()];
    let got = store.sessions().upsert(&hop).await.expect("upsert jump");
    assert_eq!(got.jump_chain, vec!["jump-a", "jump-b"]);

    let def = core_store::TunnelRecord {
        id: "td-1".into(),
        session_id: "s1".into(),
        kind: "local".into(),
        bind_host: "127.0.0.1".into(),
        bind_port: 13306,
        target_host: Some("10.0.0.8".into()),
        target_port: Some(3306),
        autostart: true,
        with_session: false,
        created_at: String::new(),
    };
    store.tunnels().upsert(&def).await.expect("tunnel upsert");
    let all = store.tunnels().list().await.expect("list");
    assert_eq!(all.len(), 1);
    assert!(all[0].autostart);
    assert!(!all[0].with_session);
    let for_s = store
        .tunnels()
        .for_session("s1")
        .await
        .expect("for_session");
    assert_eq!(for_s.len(), 1);

    // 会话删除级联隧道定义（FK ON DELETE CASCADE）
    store.sessions().delete("s1").await.expect("delete session");
    assert!(store.tunnels().list().await.expect("list").is_empty());

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn config_export_import_roundtrip() {
    let path = temp_db("export");
    let store = Store::open(&path).await.expect("open");
    let mut rec = sample("s1", "导出源");
    rec.jump_chain = vec!["hop-x".into()];
    store.sessions().upsert(&rec).await.expect("upsert");
    store
        .credentials()
        .put(
            "s1",
            core_store::CredentialKind::Password,
            &core_store::Secret::new(b"s3cret".to_vec()),
        )
        .await
        .expect("put cred");
    store
        .tunnels()
        .upsert(&core_store::TunnelRecord {
            id: "td-1".into(),
            session_id: "s1".into(),
            kind: "local".into(),
            bind_host: "127.0.0.1".into(),
            bind_port: 13306,
            target_host: Some("10.0.0.8".into()),
            target_port: Some(3306),
            autostart: true,
            with_session: true,
            created_at: String::new(),
        })
        .await
        .expect("tunnel");

    // 明文：不含秘密
    let plain = core_store::export_plain(&store)
        .await
        .expect("export plain");
    assert!(plain.contains("导出源"));
    assert!(!plain.contains("s3cret"), "明文导出绝不含秘密材料");

    // 加密：含凭据
    let enc = core_store::export_encrypted(&store, b"passphrase-1")
        .await
        .expect("export enc");
    assert!(enc.contains("\"encrypted\": true"));
    assert!(!enc.contains("s3cret"));

    // 导入到全新库
    let path2 = temp_db("import");
    let store2 = Store::open(&path2).await.expect("open2");
    let out = core_store::import_config(&store2, &plain, None)
        .await
        .expect("import plain");
    assert_eq!(out.sessions, 1);
    assert_eq!(out.tunnels, 1);
    assert_eq!(out.credentials, 0);
    let got = store2.sessions().get("s1").await.expect("get");
    assert_eq!(got.jump_chain, vec!["hop-x"]);

    // 错误口令必须失败（不能静默导入）
    let err = core_store::import_config(&store2, &enc, Some(b"wrong")).await;
    assert!(err.is_err(), "错误口令必须报错");

    let out2 = core_store::import_config(&store2, &enc, Some(b"passphrase-1"))
        .await
        .expect("import enc");
    assert_eq!(out2.credentials, 1);
    let sec = store2.credentials().get("s1").await.expect("cred back");
    assert_eq!(sec.expose(), b"s3cret");

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    let _ = std::fs::remove_dir_all(path2.parent().unwrap());
}

#[tokio::test]
async fn transfer_history_crud() {
    let path = temp_db("transfer");
    let store = Store::open(&path).await.expect("open");
    store
        .sessions()
        .upsert(&sample("s1", "A"))
        .await
        .expect("session");

    let repo = store.transfers();
    let rec = core_store::TransferRecord {
        id: "tr-1".into(),
        session_id: "s1".into(),
        direction: "download".into(),
        local: "C:/tmp/a.bin".into(),
        remote: "/var/a.bin".into(),
        bytes_done: 50,
        bytes_total: 100,
        state: "paused".into(),
        error: None,
        updated_at: String::new(),
    };
    repo.upsert(&rec).await.expect("insert");

    // upsert 覆盖：断点前进 + 终态
    let mut rec2 = rec.clone();
    rec2.bytes_done = 100;
    rec2.state = "done".into();
    repo.upsert(&rec2).await.expect("update");
    let list = repo.for_session("s1").await.expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].bytes_done, 100);
    assert_eq!(list[0].state, "done");
    assert!(
        !list[0].updated_at.is_empty(),
        "updated_at 由 SQLite 时钟生成"
    );

    // clear_settled：done/canceled 清除，failed 保留
    let mut rec3 = rec.clone();
    rec3.id = "tr-2".into();
    rec3.state = "failed".into();
    rec3.error = Some("E5004 传输中断".into());
    repo.upsert(&rec3).await.expect("insert2");
    let cleared = repo.clear_settled("s1").await.expect("clear");
    assert_eq!(cleared, 1);
    let rest = repo.for_session("s1").await.expect("list2");
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].id, "tr-2");
}

// ---------- 批次二：分组管理（groupPath 前缀改写，事务化） ----------

fn in_group(id: &str, name: &str, group: &str) -> SessionRecord {
    SessionRecord {
        group_path: group.into(),
        ..sample(id, name)
    }
}

async fn group_of(store: &Store, id: &str) -> String {
    store.sessions().get(id).await.expect("get").group_path
}

#[tokio::test]
async fn group_rename_nested_and_guards() {
    let path = temp_db("grename");
    let store = Store::open(&path).await.expect("open");
    let s = store.sessions();
    s.upsert(&in_group("s1", "华东-1", "生产/华东"))
        .await
        .expect("u1");
    s.upsert(&in_group("s2", "华东-子", "生产/华东/子组"))
        .await
        .expect("u2");
    s.upsert(&in_group("s3", "华南-1", "生产/华南"))
        .await
        .expect("u3");

    // 嵌套重命名：直属与子树一起改写，兄弟分组不动
    let n = s
        .group_rename("生产/华东", "运维/东区")
        .await
        .expect("rename");
    assert_eq!(n, 2);
    assert_eq!(group_of(&store, "s1").await, "运维/东区");
    assert_eq!(group_of(&store, "s2").await, "运维/东区/子组");
    assert_eq!(group_of(&store, "s3").await, "生产/华南");

    // 循环防护：目标落在源子树内 → 拒绝且无改写
    assert!(matches!(
        s.group_rename("生产", "生产/华东").await,
        Err(core_store::StoreError::Validation(_))
    ));
    assert_eq!(group_of(&store, "s3").await, "生产/华南");

    // 自改名 = 空操作
    assert_eq!(s.group_rename("生产", "生产").await.expect("noop"), 0);

    // 移到未分组（new=''）：直属成员进根，子树提升为顶级
    let n = s.group_rename("运维/东区", "").await.expect("to root");
    assert_eq!(n, 2);
    assert_eq!(group_of(&store, "s1").await, "");
    assert_eq!(group_of(&store, "s2").await, "子组");

    // 非法路径拒绝
    assert!(s.group_rename("", "x").await.is_err());
    assert!(s.group_rename("a", "b//c").await.is_err());
    assert!(s.group_rename("a", " b").await.is_err());

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn group_delete_keep_sessions() {
    let path = temp_db("gdelkeep");
    let store = Store::open(&path).await.expect("open");
    let s = store.sessions();
    s.upsert(&in_group("s1", "直属", "生产/华东"))
        .await
        .expect("u1");
    s.upsert(&in_group("s2", "子组机", "生产/华东/子组"))
        .await
        .expect("u2");
    s.upsert(&in_group("s3", "生产根", "生产"))
        .await
        .expect("u3");

    // 非空分组删除（保留会话）：直属 → 未分组；子分组上移父级
    let n = s.group_delete("生产/华东", false).await.expect("del");
    assert_eq!(n, 2);
    assert_eq!(group_of(&store, "s1").await, "");
    assert_eq!(group_of(&store, "s2").await, "生产/子组");
    assert_eq!(group_of(&store, "s3").await, "生产");

    // 删顶级分组：子树提升到根
    let n = s.group_delete("生产", false).await.expect("del root");
    assert_eq!(n, 2);
    assert_eq!(group_of(&store, "s2").await, "子组");
    assert_eq!(group_of(&store, "s3").await, "");

    assert!(s.group_delete("", false).await.is_err());

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn group_delete_with_sessions_cascades_credentials() {
    let path = temp_db("gdelcascade");
    let store = Store::open(&path).await.expect("open");
    let s = store.sessions();
    s.upsert(&in_group("s1", "连带删", "生产/华东"))
        .await
        .expect("u1");
    s.upsert(&in_group("s2", "保留", "生产/华南"))
        .await
        .expect("u2");
    store
        .credentials()
        .put("s1", CredentialKind::Password, &Secret::new(b"pw".to_vec()))
        .await
        .expect("cred");

    let n = s.group_delete("生产/华东", true).await.expect("del");
    assert_eq!(n, 1);
    // 会话与凭据级联删除
    assert!(matches!(
        s.get("s1").await,
        Err(core_store::StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.credentials().get("s1").await,
        Err(core_store::StoreError::NotFound(_))
    ));
    // 兄弟分组不受影响
    assert_eq!(group_of(&store, "s2").await, "生产/华南");

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn move_to_group_batch() {
    let path = temp_db("gmove");
    let store = Store::open(&path).await.expect("open");
    let s = store.sessions();
    s.upsert(&in_group("s1", "a", "生产")).await.expect("u1");
    s.upsert(&in_group("s2", "b", "生产")).await.expect("u2");
    s.upsert(&in_group("s3", "c", "测试")).await.expect("u3");

    let n = s
        .move_to_group(&["s1".into(), "s2".into(), "不存在".into()], "归档/2026")
        .await
        .expect("move");
    assert_eq!(n, 2, "不存在的 id 不计入");
    assert_eq!(group_of(&store, "s1").await, "归档/2026");
    assert_eq!(group_of(&store, "s2").await, "归档/2026");
    assert_eq!(group_of(&store, "s3").await, "测试");

    // 移到未分组 + 空列表 + 非法路径
    assert_eq!(
        s.move_to_group(&["s1".into()], "").await.expect("to root"),
        1
    );
    assert_eq!(group_of(&store, "s1").await, "");
    assert_eq!(s.move_to_group(&[], "x").await.expect("empty"), 0);
    assert!(s.move_to_group(&["s1".into()], "a//b").await.is_err());

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
