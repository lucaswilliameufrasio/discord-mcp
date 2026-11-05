use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Form, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::StorageConfig;

struct Access {
    code: Option<String>,
    expires: Instant,
    session: Option<String>,
}

struct App {
    storage: StorageConfig,
    origin: String,
    host: String,
    access: Mutex<Access>,
}

type Shared = Arc<App>;

fn random_token() -> Result<String, &'static str> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "random source unavailable")?;
    Ok(hex::encode(bytes))
}

pub async fn run(storage: StorageConfig) -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let code = random_token()?;
    let app = Arc::new(App {
        storage,
        origin: format!("http://{addr}"),
        host: addr.to_string(),
        access: Mutex::new(Access {
            code: Some(code.clone()),
            expires: Instant::now() + Duration::from_secs(300),
            session: None,
        }),
    });
    // The code is shown only to the local operator, never sent to the browser or logs by the server.
    eprintln!(
        "Interface local: http://{addr}/\nCódigo de acesso (válido por 5 minutos, uso único): {code}"
    );
    axum::serve(listener, router(app)).await?;
    Ok(())
}

fn router(app: Shared) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/login", post(login))
        .route("/channels", post(save))
        .route("/channels/bot", post(save_bot))
        .route("/delete", post(delete_confirm))
        .route("/delete/confirm", post(delete))
        .layer(DefaultBodyLimit::max(8192))
        .with_state(app)
}

fn page(status: StatusCode, body: String) -> Response {
    let document = format!(
        r#"<!doctype html><html lang="pt-BR"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Discord MCP · Canais</title><style>
    :root{{color-scheme:dark;font-family:system-ui,sans-serif;background:#101a2d;color:#edf3ff}}body{{margin:0}}main{{max-width:860px;margin:auto;padding:2rem 1.25rem}}header{{border-bottom:1px solid #52637e;margin-bottom:2rem}}h1{{font-size:clamp(2rem,5vw,3rem);margin:.4rem 0}}h2{{font-size:1.3rem}}p{{line-height:1.6;color:#c5d1e5}}section{{margin:2rem 0}}label{{display:block;font-weight:600;margin:1rem 0 .4rem}}input{{box-sizing:border-box;width:100%;padding:.8rem;border:1px solid #7c91b1;border-radius:5px;background:#1c2b43;color:white;font:inherit}}button{{margin-top:1rem;padding:.7rem 1rem;border:0;border-radius:5px;background:#a9c4ff;color:#102343;font-weight:700;cursor:pointer}}button.danger{{background:#ffb4b3;color:#4e111a}}:focus-visible{{outline:3px solid #fbd279;outline-offset:3px}}ul{{padding:0;list-style:none}}li{{display:flex;align-items:center;justify-content:space-between;gap:1rem;border-bottom:1px solid #52637e;padding:.8rem 0}}li button{{margin:0}}.note{{font-size:.9rem}}a{{color:#bdd2ff}}
    </style></head><body><main><header><p>Discord MCP · configuração local</p><h1>Canais</h1></header>{body}</main></body></html>"#
    );
    let mut response = (status, Html(document)).into_response();
    let h = response.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("content-security-policy", HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
}

fn allowed_host(app: &App, headers: &HeaderMap) -> bool {
    headers.get(header::HOST).and_then(|v| v.to_str().ok()) == Some(app.host.as_str())
}

fn allowed_post(app: &App, headers: &HeaderMap) -> bool {
    allowed_host(app, headers)
        && headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == Some(app.origin.as_str())
}

fn same_secret(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn authenticated(app: &App, headers: &HeaderMap) -> bool {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let given = cookie
        .split(';')
        .filter_map(|v| v.trim().split_once('='))
        .find_map(|(name, value)| {
            if name == "discord_mcp_session" {
                Some(value)
            } else {
                None
            }
        });
    let access = match app.access.lock() {
        Ok(access) => access,
        Err(_) => return false,
    };
    match (given, access.session.as_deref()) {
        (Some(a), Some(b)) => same_secret(a, b),
        _ => false,
    }
}

fn denied() -> Response {
    page(StatusCode::FORBIDDEN, "<p>Acesso negado.</p>".into())
}

async fn home(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if !allowed_host(&app, &headers) {
        return denied();
    }
    if !authenticated(&app, &headers) {
        return page(StatusCode::OK, r#"<p>Insira o código exibido no terminal onde a interface foi iniciada.</p><form method="post" action="/login"><label for="code">Código de acesso</label><input id="code" name="code" type="password" required autocomplete="off"><button>Entrar</button></form>"#.into());
    }
    let store = match app.storage.open() {
        Ok(store) => store,
        Err(_) => {
            return page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "<p>Não foi possível abrir o banco de canais.</p>".into(),
            );
        }
    };
    let names = match store.list() {
        Ok(names) => names,
        Err(_) => {
            return page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "<p>Não foi possível listar os canais.</p>".into(),
            );
        }
    };
    let mut list = String::new();
    for name in names {
        let name = escape_html(&name);
        list.push_str(&format!(r#"<li><span>{name}</span><form method="post" action="/delete"><input type="hidden" name="name" value="{name}"><button class="danger">Remover</button></form></li>"#));
    }
    if list.is_empty() {
        list.push_str("<li>Nenhum canal cadastrado.</li>");
    }
    page(
        StatusCode::OK,
        format!(
            r#"<p>Gerencie os destinos locais. Webhooks e tokens cadastrados não são exibidos novamente.</p><section><h2>Canais cadastrados</h2><ul>{list}</ul></section><section><h2>Cadastrar ou substituir webhook</h2><p class="note">Use letras, números, hífen ou sublinhado no nome. Um nome existente será substituído.</p><form method="post" action="/channels"><label for="name">Nome do canal</label><input id="name" name="name" maxlength="64" pattern="[A-Za-z0-9_-]+" required><label for="webhook">URL do webhook do Discord</label><input id="webhook" name="webhook" type="password" autocomplete="off" required><button>Salvar webhook</button></form></section><section><h2>Cadastrar ou substituir bot</h2><p>O bot precisa estar instalado no servidor e ter permissões de leitura e envio para o canal. Um nome existente será substituído.</p><form method="post" action="/channels/bot"><label for="bot-name">Nome do canal</label><input id="bot-name" name="name" maxlength="64" pattern="[A-Za-z0-9_-]+" required><label for="channel-id">ID do canal Discord</label><input id="channel-id" name="channel_id" inputmode="numeric" pattern="[0-9]+" required><label for="bot-token">Token do bot</label><input id="bot-token" name="token" type="password" autocomplete="off" required><button>Salvar bot</button></form></section>"#
        ),
    )
}

#[derive(Deserialize)]
struct Login {
    code: String,
}

async fn login(State(app): State<Shared>, headers: HeaderMap, Form(form): Form<Login>) -> Response {
    if !allowed_post(&app, &headers) {
        return denied();
    }
    let mut access = match app.access.lock() {
        Ok(access) => access,
        Err(_) => return denied(),
    };
    let valid = Instant::now() < access.expires
        && access
            .code
            .as_deref()
            .is_some_and(|code| same_secret(code, &form.code));
    if !valid {
        return denied();
    }
    let session = match random_token() {
        Ok(value) => value,
        Err(_) => return denied(),
    };
    access.code = None;
    access.session = Some(session.clone());
    let mut response = (StatusCode::SEE_OTHER, [(header::LOCATION, "/")]).into_response();
    let cookie = format!("discord_mcp_session={session}; HttpOnly; SameSite=Strict; Path=/");
    match HeaderValue::from_str(&cookie) {
        Ok(value) => {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
        Err(_) => return denied(),
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Deserialize)]
struct ChannelForm {
    name: String,
    webhook: String,
}

#[derive(Deserialize)]
struct BotForm {
    name: String,
    channel_id: String,
    token: String,
}

async fn save_bot(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(form): Form<BotForm>,
) -> Response {
    if !allowed_post(&app, &headers) || !authenticated(&app, &headers) {
        return denied();
    }
    if app
        .storage
        .open()
        .and_then(|store| store.put_bot(&form.name, &form.channel_id, &form.token))
        .is_err()
    {
        return page(StatusCode::UNPROCESSABLE_ENTITY, "<p>Não foi possível salvar o bot. Confira nome, ID, token e banco.</p><p><a href=\"/\">Voltar aos canais</a></p>".into());
    }
    (StatusCode::SEE_OTHER, [(header::LOCATION, "/")]).into_response()
}

async fn save(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(form): Form<ChannelForm>,
) -> Response {
    if !allowed_post(&app, &headers) || !authenticated(&app, &headers) {
        return denied();
    }
    let result = app
        .storage
        .open()
        .and_then(|store| store.put_webhook(&form.name, &form.webhook));
    if result.is_err() {
        return page(StatusCode::UNPROCESSABLE_ENTITY, "<p>Não foi possível salvar. Confira o nome, a URL e a configuração do banco.</p><p><a href=\"/\">Voltar aos canais</a></p>".into());
    }
    (StatusCode::SEE_OTHER, [(header::LOCATION, "/")]).into_response()
}

#[derive(Deserialize)]
struct NameForm {
    name: String,
}

async fn delete_confirm(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(form): Form<NameForm>,
) -> Response {
    if !allowed_post(&app, &headers) || !authenticated(&app, &headers) {
        return denied();
    }
    // Only names present in the store (and validated on insertion) are rendered.
    let names = match app.storage.open().and_then(|store| store.list()) {
        Ok(names) => names,
        Err(_) => return denied(),
    };
    if !names.contains(&form.name) {
        return page(StatusCode::NOT_FOUND, "<p>Canal não encontrado.</p>".into());
    }
    let safe_name = escape_html(&form.name);
    page(
        StatusCode::OK,
        format!(
            r#"<p>Remover o canal <strong>{}</strong>? Essa ação remove o cadastro local, mas não revoga o webhook no Discord.</p><form method="post" action="/delete/confirm"><input type="hidden" name="name" value="{}"><button class="danger">Confirmar remoção</button></form><p><a href="/">Cancelar</a></p>"#,
            safe_name, safe_name
        ),
    )
}

async fn delete(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(form): Form<NameForm>,
) -> Response {
    if !allowed_post(&app, &headers) || !authenticated(&app, &headers) {
        return denied();
    }
    if app
        .storage
        .open()
        .and_then(|store| store.delete(&form.name))
        .is_err()
    {
        return page(
            StatusCode::NOT_FOUND,
            "<p>Não foi possível remover o canal.</p>".into(),
        );
    }
    (StatusCode::SEE_OTHER, [(header::LOCATION, "/")]).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Client;

    #[tokio::test]
    async fn local_web_requires_session_and_origin_and_manages_real_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let storage = StorageConfig {
            path: dir.path().join("test.db"),
            key: [12; 32],
            audit_retention_days: 90,
        };
        storage.open().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let origin = format!("http://{host}");
        let code = "a".repeat(64);
        let app = Arc::new(App {
            storage: storage.clone(),
            origin: origin.clone(),
            host,
            access: Mutex::new(Access {
                code: Some(code.clone()),
                expires: Instant::now() + Duration::from_secs(60),
                session: None,
            }),
        });
        let server = tokio::spawn(async move { axum::serve(listener, router(app)).await.unwrap() });
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let form = "name=alerts&webhook=https%3A%2F%2Fdiscord.com%2Fapi%2Fwebhooks%2F123%2Fsynthetic-token";
        let post = |path: &str, body: String| {
            client
                .post(format!("{origin}{path}"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(body)
        };
        let unauthorized = post("/channels", form.into())
            .header(header::ORIGIN, &origin)
            .send()
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::FORBIDDEN);
        let wrong_origin = post("/login", format!("code={code}"))
            .header(header::ORIGIN, "http://evil.invalid")
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_origin.status(), StatusCode::FORBIDDEN);
        let bad_host = client
            .get(&origin)
            .header(header::HOST, "evil.invalid")
            .send()
            .await
            .unwrap();
        assert_eq!(bad_host.status(), StatusCode::FORBIDDEN);
        let login = post("/login", format!("code={code}"))
            .header(header::ORIGIN, &origin)
            .send()
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::SEE_OTHER);
        let cookie = login
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let replay = post("/login", format!("code={code}"))
            .header(header::ORIGIN, &origin)
            .send()
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::FORBIDDEN);
        let saved = post("/channels", form.into())
            .header(header::ORIGIN, &origin)
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(saved.status(), StatusCode::SEE_OTHER);
        let bot_saved = post(
            "/channels/bot",
            "name=robot&channel_id=123&token=synthetic-bot-token".into(),
        )
        .header(header::ORIGIN, &origin)
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
        assert_eq!(bot_saved.status(), StatusCode::SEE_OTHER);
        let page = client
            .get(&origin)
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(page.contains("alerts"));
        assert!(!page.contains("synthetic-token"));
        assert!(!page.contains("synthetic-bot-token"));
        assert_eq!(
            storage.open().unwrap().list().unwrap(),
            vec!["alerts", "robot"]
        );
        let confirmation = post("/delete", "name=alerts".into())
            .header(header::ORIGIN, &origin)
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(confirmation.status(), StatusCode::OK);
        assert_eq!(
            storage.open().unwrap().list().unwrap(),
            vec!["alerts", "robot"]
        );
        let removed = post("/delete/confirm", "name=alerts".into())
            .header(header::ORIGIN, &origin)
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(removed.status(), StatusCode::SEE_OTHER);
        assert_eq!(storage.open().unwrap().list().unwrap(), vec!["robot"]);
        server.abort();
    }
}
