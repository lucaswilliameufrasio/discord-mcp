use discord_mcp::channels::{AuthorizationExchange, AuthorizationGrant, ChannelStore, StoreError};

const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const REDIRECT: &str = "http://127.0.0.1:8765/callback";
const RESOURCE: &str = "https://example.invalid/mcp";

fn grant() -> AuthorizationGrant<'static> {
    AuthorizationGrant {
        user_id: "123",
        client_id: "registered-client",
        redirect_uri: REDIRECT,
        resource: RESOURCE,
        code_challenge: CHALLENGE,
    }
}

fn exchange(code: &str) -> AuthorizationExchange<'_> {
    AuthorizationExchange {
        code,
        client_id: "registered-client",
        redirect_uri: REDIRECT,
        resource: RESOURCE,
        code_verifier: VERIFIER,
    }
}

#[test]
fn pkce_code_is_bound_to_client_redirect_resource_and_single_use() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oauth.db");
    let store = ChannelStore::open(&path, &[31; 32]).unwrap();
    let code = store.issue_authorization_code(&grant(), 1000).unwrap();
    let mut request = exchange(&code);
    request.code_verifier = "x";
    assert!(matches!(
        store.exchange_authorization_code(&request, 1001),
        Err(StoreError::AuthorizationDenied)
    ));
    request = exchange(&code);
    request.code_verifier = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert!(matches!(
        store.exchange_authorization_code(&request, 1001),
        Err(StoreError::AuthorizationDenied)
    ));
    request = exchange(&code);
    request.client_id = "different-client";
    assert!(matches!(
        store.exchange_authorization_code(&request, 1001),
        Err(StoreError::AuthorizationDenied)
    ));
    request = exchange(&code);
    request.redirect_uri = "http://127.0.0.1:8765/other";
    assert!(matches!(
        store.exchange_authorization_code(&request, 1001),
        Err(StoreError::AuthorizationDenied)
    ));
    request = exchange(&code);
    request.resource = "https://other.invalid/mcp";
    assert!(matches!(
        store.exchange_authorization_code(&request, 1001),
        Err(StoreError::AuthorizationDenied)
    ));
    assert!(matches!(
        store.exchange_authorization_code(&exchange(&code), 999),
        Err(StoreError::AuthorizationDenied)
    ));
    let token = store
        .exchange_authorization_code(&exchange(&code), 1001)
        .unwrap();
    assert_eq!(
        store
            .verify_access_token(&token, RESOURCE, 1001)
            .unwrap()
            .as_deref(),
        Some("123")
    );
    assert!(matches!(
        store.exchange_authorization_code(&exchange(&code), 1002),
        Err(StoreError::AuthorizationDenied)
    ));
    let expired = store.issue_authorization_code(&grant(), 1000).unwrap();
    assert!(matches!(
        store.exchange_authorization_code(&exchange(&expired), 1300),
        Err(StoreError::AuthorizationDenied)
    ));
    drop(store);
    let bytes = std::fs::read(path).unwrap();
    assert!(!bytes.windows(code.len()).any(|v| v == code.as_bytes()));
    assert!(!bytes.windows(token.len()).any(|v| v == token.as_bytes()));
}

#[test]
fn concurrent_exchange_has_exactly_one_winner() {
    use std::sync::{Arc, Barrier};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oauth.db");
    let key = [32; 32];
    let store = ChannelStore::open(&path, &key).unwrap();
    let code = store.issue_authorization_code(&grant(), 1000).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let path = path.clone();
        let code = code.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            let store = ChannelStore::open(&path, &key).unwrap();
            barrier.wait();
            store
                .exchange_authorization_code(&exchange(&code), 1001)
                .is_ok()
        }));
    }
    let outcomes: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|v| **v).count(), 1);
}
