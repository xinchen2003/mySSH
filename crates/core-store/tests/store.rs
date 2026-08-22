//! core-store 集成测试：真实 SQLite 临时库，CRUD/级联/审计游标/DPAPI 凭据回环。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use core_store::{Actor, AuthType, CredentialKind, Secret, SessionRecord, Store};

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
