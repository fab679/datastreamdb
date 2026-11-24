//! Admin CLI flows over the wire: add/del/mod/info/chmod via `AclAdmin`.

#[path = "common/mod.rs"]
mod common;

use common::{boot_alpha_acl, boot_zero, channel};
use datastreamdb_proto::api::{LoginRequest, data_stream_db_client::DataStreamDbClient};
use datastreamdb_server::{AclAdmin, GuardianCreds};

fn secret() -> Vec<u8> {
    vec![11u8; 64]
}

async fn admin(addr: &str) -> AclAdmin {
    AclAdmin::connect(
        addr,
        &GuardianCreds {
            user: "groot".to_owned(),
            password: "password".to_owned(),
            namespace: 0,
        },
    )
    .await
    .expect("connect")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acl_admin_flows() {
    let (_zdir, zgrpc, _) = boot_zero(1).await;
    let (_adir, agrpc, _) = boot_alpha_acl(&zgrpc, 1, true, secret()).await;
    let mut admin = admin(&agrpc).await;

    // Add + conflict.
    admin.add_user("bob", "bob12345").await.expect("add user");
    assert!(admin.add_user("bob", "other123").await.is_err());
    admin.add_group("dev").await.expect("add group");
    assert!(admin.add_group("dev").await.is_err());

    // Info round-trips.
    let user = admin.user("bob").await.expect("info").expect("bob");
    assert_eq!(user.user_id, "bob");
    assert!(admin.user("ghost").await.expect("info").is_none());
    let group = admin.group("dev").await.expect("info").expect("dev");
    assert_eq!(group.group_id, "dev");

    // Group membership diff (add, idempotent re-set, clear).
    admin
        .set_user_groups("bob", &["dev".to_owned()])
        .await
        .expect("join");
    let user = admin.user("bob").await.expect("info").expect("bob");
    assert_eq!(user.groups.len(), 1);
    admin
        .set_user_groups("bob", &["dev".to_owned()])
        .await
        .expect("idempotent");
    admin.set_user_groups("bob", &[]).await.expect("leave");
    let user = admin.user("bob").await.expect("info").expect("bob");
    assert!(user.groups.is_empty());
    admin
        .set_user_groups("bob", &["dev".to_owned()])
        .await
        .expect("rejoin");

    // Rules: create, update, delete (negative perm).
    admin.chmod("dev", "name", 4).await.expect("rule add");
    let group = admin.group("dev").await.expect("info").expect("dev");
    assert_eq!(group.rules.len(), 1);
    assert_eq!(group.rules[0].perm, 4);
    admin.chmod("dev", "name", 6).await.expect("rule update");
    let group = admin.group("dev").await.expect("info").expect("dev");
    assert_eq!(group.rules[0].perm, 6);
    admin.chmod("dev", "name", -1).await.expect("rule del");
    let group = admin.group("dev").await.expect("info").expect("dev");
    assert!(group.rules.is_empty());
    assert!(admin.chmod("dev", "name", 8).await.is_err());
    assert!(admin.chmod("ghost", "name", 4).await.is_err());

    // Password change takes effect at login.
    admin
        .change_password("bob", "rotated99")
        .await
        .expect("change");
    let mut anon = DataStreamDbClient::new(channel(&agrpc).await);
    assert!(
        anon.login(LoginRequest {
            userid: "bob".to_owned(),
            password: "bob12345".to_owned(),
            ..LoginRequest::default()
        })
        .await
        .is_err()
    );
    anon.login(LoginRequest {
        userid: "bob".to_owned(),
        password: "rotated99".to_owned(),
        ..LoginRequest::default()
    })
    .await
    .expect("login with new password");

    // Delete user then group; unknown ids fail.
    admin.del_user("bob").await.expect("del user");
    assert!(admin.user("bob").await.expect("info").is_none());
    assert!(admin.del_user("bob").await.is_err());
    admin.del_group("dev").await.expect("del group");
    assert!(admin.group("dev").await.expect("info").is_none());
    assert!(admin.del_group("dev").await.is_err());
}
