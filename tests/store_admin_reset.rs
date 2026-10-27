//! Removing the admin password from the store.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use nanoleaf2mqtt::store::Store;

#[tokio::test]
async fn clearing_the_admin_password_allows_the_first_start_again() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    assert!(
        !store.clear_admin_password().await.unwrap(),
        "nothing to remove is fine"
    );

    assert!(
        store
            .set_initial_admin_password_hash("hash-1")
            .await
            .unwrap()
    );
    assert!(
        !store
            .set_initial_admin_password_hash("hash-2")
            .await
            .unwrap()
    );
    assert!(store.clear_admin_password().await.unwrap());
    assert_eq!(store.admin_password_hash().await.unwrap(), None);

    assert!(
        store
            .set_initial_admin_password_hash("hash-3")
            .await
            .unwrap()
    );
    assert_eq!(
        store.admin_password_hash().await.unwrap().as_deref(),
        Some("hash-3")
    );
}
