//! Shared administration: Discord login plus an operator-configured admin identity.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Form, Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::{
    channels::{AuditActor, AuditContext, ChannelStore, LoginKind, current_timestamp},
    cli_authorization::{CliAuthorizations, PermissionChange},
    discord_login::{DiscordLoginClient, PendingDiscordLogin},
    http_auth::AuthState,
};

const LIMIT: usize = 1024;

#[derive(Clone)]
pub struct AdminConfig {
    pub auth: AuthState,
    pub public_origin: String,
    pub admin_user_id: String,
    pub discord: DiscordLoginClient,
}

struct Attempt {
    login: PendingDiscordLogin,
    expires: Instant,
}
struct Inner {
    config: AdminConfig,
    attempts: Mutex<HashMap<String, Attempt>>,
    sessions: Mutex<HashMap<String, Instant>>,
    cli: CliAuthorizations,
}
type Shared = Arc<Inner>;

pub fn router(config: AdminConfig) -> Result<Router, &'static str> {
    if config.admin_user_id.is_empty()
        || config.admin_user_id.len() > 20
        || !config.admin_user_id.bytes().all(|v| v.is_ascii_digit())
        || config.discord.redirect_uri().as_str()
            != format!("{}/admin/discord/callback", config.public_origin)
    {
        return Err("invalid admin configuration");
    }
    let app = Arc::new(Inner {
        config,
        attempts: Mutex::new(HashMap::new()),
        sessions: Mutex::new(HashMap::new()),
        cli: CliAuthorizations::default(),
    });
    Ok(Router::new()
        .route("/admin", get(home))
        .route("/admin/channels", get(channels))
        .route("/admin/observability", get(observability))
        .route("/admin/audit", get(audit))
        .route("/admin/login", get(login))
        .route("/admin/discord/callback", get(callback))
        .route("/admin/channel", get(channel))
        .route("/admin/permissions", post(set_permission))
        .route("/admin/logout", post(logout))
        .route("/v1/cli/requests", post(cli_begin))
        .route("/v1/cli/execute", post(cli_execute))
        .route("/admin/cli", get(cli_consent).post(cli_approve))
        .layer(DefaultBodyLimit::max(8192))
        .with_state(app))
}

#[derive(Deserialize)]
struct CliBegin {
    user_id: String,
    channel: String,
    read: bool,
    send: bool,
    challenge: String,
}

async fn cli_begin(State(app): State<Shared>, Json(req): Json<CliBegin>) -> Response {
    let Ok(bytes) = hex::decode(&req.challenge) else {
        return error(StatusCode::BAD_REQUEST);
    };
    let Ok(challenge) = <[u8; 32]>::try_from(bytes) else {
        return error(StatusCode::BAD_REQUEST);
    };
    let operation = PermissionChange {
        user_id: req.user_id,
        channel: req.channel,
        read: req.read,
        send: req.send,
    };
    let id = match app.cli.begin(operation, challenge, Instant::now()) {
        Ok(id) => id,
        Err("invalid operation") => return error(StatusCode::UNPROCESSABLE_ENTITY),
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE),
    };
    let mut response = Json(rmcp::serde_json::json!({"request_id": id})).into_response();
    headers(&mut response);
    response
}

#[derive(Deserialize)]
struct CliRequestId {
    request_id: String,
}

async fn cli_consent(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(req): Query<CliRequestId>,
) -> Response {
    if !authorized(&app, &headers) {
        return page("<p>Entre como administrador em <a href=\"/admin/login\">Login Discord</a> e reabra o link da CLI.</p>".into());
    }
    let Some(operation) = app.cli.pending(&req.request_id, Instant::now()) else {
        return error(StatusCode::NOT_FOUND);
    };
    page(format!(
        "<h1>Autorizar solicitação da CLI</h1><p>Usuário: {}<br>Canal: {}<br>Leitura: {}<br>Envio: {}</p><form method=\"post\" action=\"/admin/cli\"><input type=\"hidden\" name=\"request_id\" value=\"{}\"><button>Autorizar esta alteração</button></form>",
        escape(&operation.user_id),
        escape(&operation.channel),
        operation.read,
        operation.send,
        escape(&req.request_id)
    ))
}

async fn cli_approve(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(req): Form<CliRequestId>,
) -> Response {
    if !allowed_post(&app, &headers) {
        return error(StatusCode::FORBIDDEN);
    }
    if !app.cli.approve(&req.request_id, Instant::now()) {
        return error(StatusCode::NOT_FOUND);
    }
    page("<p>Autorização concedida. Volte para a CLI.</p>".into())
}

#[derive(Deserialize)]
struct CliExecute {
    request_id: String,
    verifier: String,
}

async fn cli_execute(State(app): State<Shared>, Json(req): Json<CliExecute>) -> Response {
    let Ok(verifier) = hex::decode(req.verifier) else {
        return error(StatusCode::FORBIDDEN);
    };
    let Some(operation) = app.cli.consume(&req.request_id, &verifier, Instant::now()) else {
        return error(StatusCode::FORBIDDEN);
    };
    let Ok(now) = current_timestamp() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR);
    };
    match store(&app).and_then(|store| {
        store.set_permissions_audited(
            &operation.user_id,
            &operation.channel,
            operation.read,
            operation.send,
            AuditContext {
                actor: AuditActor::Discord(&app.config.admin_user_id),
                now,
                retention_days: app.config.auth.audit.retention_days(),
            },
        )
    }) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(crate::channels::StoreError::Storage | crate::channels::StoreError::Decryption) => {
            app.config.auth.audit.mark_failure();
            error(StatusCode::INTERNAL_SERVER_ERROR)
        }
        Err(_) => error(StatusCode::UNPROCESSABLE_ENTITY),
    }
}

async fn login(State(app): State<Shared>) -> Response {
    let (pending, url) = match app.config.discord.begin() {
        Ok(value) => value,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let state = match url.query_pairs().find(|(key, _)| key == "state") {
        Some((_, value)) => value.into_owned(),
        None => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let binding = pending.browser_binding().to_owned();
    let mut attempts = match app.attempts.lock() {
        Ok(value) => value,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    attempts.retain(|_, value| value.expires > Instant::now());
    if attempts.len() >= LIMIT {
        return error(StatusCode::SERVICE_UNAVAILABLE);
    }
    attempts.insert(
        state,
        Attempt {
            login: pending,
            expires: Instant::now() + Duration::from_secs(300),
        },
    );
    let mut response = redirect(url.as_str());
    let secure = if app.config.public_origin.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "discord_mcp_admin_login={binding}; Path=/admin; HttpOnly; SameSite=Lax; Max-Age=300{secure}"
    );
    let Ok(value) = HeaderValue::from_str(&cookie) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR);
    };
    response.headers_mut().insert(header::SET_COOKIE, value);
    response
}

#[derive(Deserialize)]
struct Callback {
    state: String,
    code: String,
    iss: String,
}

async fn callback(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<Callback>,
) -> Response {
    let Some(binding) = cookie(&headers, "discord_mcp_admin_login") else {
        return error(StatusCode::FORBIDDEN);
    };
    let attempt = match app.attempts.lock() {
        Ok(mut attempts) => {
            let matches = attempts.get(&query.state).is_some_and(|attempt| {
                bool::from(
                    attempt
                        .login
                        .browser_binding()
                        .as_bytes()
                        .ct_eq(binding.as_bytes()),
                )
            });
            if !matches {
                return error(StatusCode::FORBIDDEN);
            }
            attempts.remove(&query.state)
        }
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let Some(attempt) = attempt else {
        return error(StatusCode::FORBIDDEN);
    };
    if attempt.expires <= Instant::now() {
        return error(StatusCode::FORBIDDEN);
    }
    let identity = match app
        .config
        .discord
        .finish(
            attempt.login,
            &query.state,
            &binding,
            &query.iss,
            &query.code,
        )
        .await
    {
        Ok(value) => value,
        Err(_) => {
            let _ = app.config.auth.audit.record_login(
                &app.config.auth,
                LoginKind::Admin,
                AuditActor::Unknown,
                false,
            );
            return error(StatusCode::FORBIDDEN);
        }
    };
    if identity.id != app.config.admin_user_id {
        let _ = app.config.auth.audit.record_login(
            &app.config.auth,
            LoginKind::Admin,
            AuditActor::Discord(&identity.id),
            false,
        );
        return error(StatusCode::FORBIDDEN);
    }
    let mut random = [0u8; 32];
    if getrandom::fill(&mut random).is_err() {
        return error(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let session = hex::encode(random);
    let mut sessions = match app.sessions.lock() {
        Ok(value) => value,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    sessions.retain(|_, expires| *expires > Instant::now());
    if sessions.len() >= LIMIT {
        return error(StatusCode::SERVICE_UNAVAILABLE);
    }
    if app
        .config
        .auth
        .audit
        .record_login(
            &app.config.auth,
            LoginKind::Admin,
            AuditActor::Discord(&identity.id),
            true,
        )
        .is_err()
    {
        return error(StatusCode::INTERNAL_SERVER_ERROR);
    }
    sessions.insert(session.clone(), Instant::now() + Duration::from_secs(1800));
    let secure = if app.config.public_origin.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "discord_mcp_admin_session={session}; Path=/admin; HttpOnly; SameSite=Lax; Max-Age=1800{secure}"
    );
    let mut response = redirect("/admin");
    let Ok(value) = HeaderValue::from_str(&cookie) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR);
    };
    response.headers_mut().insert(header::SET_COOKIE, value);
    response
}

fn cookie(headers: &HeaderMap, target: &str) -> Option<String> {
    if headers.get_all(header::COOKIE).iter().count() != 1 {
        return None;
    }
    let mut result = None;
    for (name, value) in headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|v| v.trim().split_once('='))
    {
        if name == target {
            if result.is_some()
                || !(43..=64).contains(&value.len())
                || !value
                    .bytes()
                    .all(|v| v.is_ascii_alphanumeric() || matches!(v, b'_' | b'-'))
            {
                return None;
            }
            result = Some(value.to_owned());
        }
    }
    result
}

fn authorized(app: &Inner, headers: &HeaderMap) -> bool {
    let Some(session) = cookie(headers, "discord_mcp_admin_session") else {
        return false;
    };
    let Ok(sessions) = app.sessions.lock() else {
        return false;
    };
    sessions
        .get(&session)
        .is_some_and(|expires| *expires > Instant::now())
}

fn allowed_post(app: &Inner, headers: &HeaderMap) -> bool {
    headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
        == Some(app.config.public_origin.as_str())
        && authorized(app, headers)
}

fn store(app: &Inner) -> Result<ChannelStore, crate::channels::StoreError> {
    ChannelStore::open(
        &app.config.auth.database_path,
        &app.config.auth.encryption_key,
    )?
    .with_audit_retention(app.config.auth.audit.retention_days())
}

async fn home(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if !authorized(&app, &headers) {
        return page("<h1>Administração</h1><p>Entre com a conta Discord configurada como administradora.</p><p><a href=\"/admin/login\">Entrar com Discord</a></p>".into());
    }
    let now = match std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_secs()).ok())
    {
        Some(now) => now,
        None => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let Some(timestamp) = chrono::DateTime::from_timestamp(now, 0) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let metrics = match store(&app).and_then(|store| store.overview(now)) {
        Ok(metrics) => metrics,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let timestamp_iso = timestamp.to_rfc3339();
    let timestamp = timestamp.format("%Y-%m-%d %H:%M:%S UTC");
    let empty = if metrics.channels == 0 {
        "<p>Nenhum canal cadastrado. Use a interface local para adicionar o primeiro canal.</p>"
    } else {
        ""
    };
    page(format!(
        r#"<header><p>Discord MCP / Administração</p><h1>Visão geral</h1><p>Resumo do cadastro e dos acessos. Atualizado em <time datetime="{timestamp_iso}">{timestamp}</time>; recarregue para atualizar.</p></header><section aria-labelledby="summary"><h2 id="summary">Configuração e acesso</h2><dl class="metrics"><div><dt>Canais cadastrados</dt><dd>{}</dd></div><div><dt>Usuários com permissões</dt><dd>{}</dd></div><div><dt>Concessões de leitura</dt><dd>{}</dd></div><div><dt>Concessões de envio</dt><dd>{}</dd></div><div><dt>Tokens MCP não expirados</dt><dd>{}</dd></div></dl>{empty}<p>Usuários são distintos; concessões contam pares de usuário e canal. Tokens não expirados não indicam conexões ativas.</p><p><a href="/admin/channels">Gerenciar permissões dos canais</a></p></section><section aria-labelledby="monitor"><h2 id="monitor">Monitoramento e auditoria</h2><p><a href="/admin/observability">Consultar tráfego e falhas MCP HTTP</a></p><p><a href="/admin/audit">Consultar histórico de auditoria</a></p><p>A saúde do Discord não é coletada. Nenhum estado de saúde é inferido a partir desta visão.</p></section><form method="post" action="/admin/logout"><button>Sair</button></form>"#,
        metrics.channels,
        metrics.permitted_users,
        metrics.read_grants,
        metrics.send_grants,
        metrics.unexpired_tokens
    ))
}

#[derive(Deserialize)]
struct AuditQuery {
    before: Option<i64>,
    kind: Option<String>,
}

async fn audit(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Response {
    if !authorized(&app, &headers) {
        return error(StatusCode::FORBIDDEN);
    }
    let filter = query.kind.as_deref().filter(|value| !value.is_empty());
    let mut events = match store(&app)
        .and_then(|store| store.audit_events(query.before, filter, 51))
    {
        Ok(value) => value,
        Err(crate::channels::StoreError::InvalidInput) => return error(StatusCode::BAD_REQUEST),
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let more = events.len() > 50;
    if more {
        events.pop();
    }
    let before = events.last().map(|event| event.id);
    let mut rows = String::new();
    for event in events {
        let at = chrono::DateTime::from_timestamp(event.occurred_at, 0)
            .map(|value| value.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "Horário inválido".into());
        let actor = match event.actor_id {
            Some(id) => format!("{}: {}", event.actor_kind, id),
            None => event.actor_kind,
        };
        let permissions = match (event.can_read, event.can_send) {
            (Some(read), Some(send)) => format!(
                "Leitura: {}; envio: {}",
                if read { "sim" } else { "não" },
                if send { "sim" } else { "não" }
            ),
            _ => "—".into(),
        };
        rows.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>", escape(&at), escape(&event.event_type), escape(&event.outcome), escape(&actor), escape(event.channel.as_deref().unwrap_or("—")), escape(event.subject_user_id.as_deref().unwrap_or("—")), escape(&permissions)));
    }
    if rows.is_empty() {
        rows.push_str("<tr><td colspan=\"7\">Nenhum evento encontrado.</td></tr>");
    }
    let next = if more {
        let mut url =
            match reqwest::Url::parse(&format!("{}/admin/audit", app.config.public_origin)) {
                Ok(value) => value,
                Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
            };
        if let Some(id) = before {
            url.query_pairs_mut().append_pair("before", &id.to_string());
        }
        if let Some(filter) = filter {
            url.query_pairs_mut().append_pair("kind", filter);
        }
        format!(
            "<p><a href=\"{}\">Eventos anteriores</a></p>",
            escape(url.as_str())
        )
    } else {
        String::new()
    };
    let warning = if app.config.auth.audit.is_degraded() {
        "<p role=\"alert\">A gravação de auditoria falhou nesta execução; o histórico pode estar incompleto.</p>"
    } else {
        ""
    };
    page(format!(
        r#"<p><a href="/admin">Voltar à visão geral</a></p><h1>Auditoria</h1>{warning}<p>Registra resultados do login Discord após validação da tentativa, alterações de canais locais e permissões bem-sucedidas. Não registra conteúdo de mensagens ou credenciais. Dados anteriores à ativação e tentativas rejeitadas antes da validação não estão incluídos.</p><form method="get" action="/admin/audit"><label for="kind">Tipo de evento</label><select id="kind" name="kind"><option value="">Todos</option><option value="admin_login">Login administrativo</option><option value="mcp_login">Login MCP</option><option value="permission_change">Alteração de permissão</option><option value="channel_configured">Canal cadastrado ou alterado</option><option value="channel_deleted">Canal removido</option><option value="channel_grants_revoked">Revogação ao alterar canal</option></select><button>Filtrar</button></form><div class="table-scroll"><table><thead><tr><th>Horário</th><th>Evento</th><th>Resultado</th><th>Ator</th><th>Canal</th><th>Usuário afetado</th><th>Permissões</th></tr></thead><tbody>{rows}</tbody></table></div>{next}"#
    ))
}

async fn observability(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if !authorized(&app, &headers) {
        return error(StatusCode::FORBIDDEN);
    }
    let now = match std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|v| i64::try_from(v.as_secs()).ok())
    {
        Some(now) => now,
        None => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let summary = match store(&app)
        .and_then(|store| store.operational_summary(now.saturating_sub(86_400).max(0), now))
    {
        Ok(value) => value,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let warning = if app.config.auth.metrics.is_degraded() {
        "<p role=\"alert\">A coleta falhou nesta execução; os dados podem estar incompletos.</p>"
    } else {
        ""
    };
    if summary.buckets == 0 {
        return page(format!(
            "<p><a href=\"/admin\">Voltar à visão geral</a></p><h1>Monitoramento</h1>{warning}<p>Nenhum dado de MCP HTTP registrado no intervalo. A ausência de dados não comprova ausência de tráfego.</p>"
        ));
    }
    let average = if summary.http_requests > 0 {
        format!("{} ms", summary.response_ms_sum / summary.http_requests)
    } else {
        "Indisponível".into()
    };
    page(format!(
        r#"<p><a href="/admin">Voltar à visão geral</a></p><h1>Monitoramento</h1>{warning}<p>Últimas 24 horas, agrupadas por minuto e atualizadas ao recarregar. Coleta de melhor esforço: dados anteriores ao início da coleta, operações canceladas e falhas de gravação podem estar ausentes.</p><dl class="metrics"><div><dt>Requisições MCP HTTP concluídas</dt><dd>{}</dd></div><div><dt>Respostas HTTP 4xx/5xx</dt><dd>{}</dd></div><div><dt>Tempo médio até resposta HTTP</dt><dd>{average}</dd></div><div><dt>Envios concluídos</dt><dd>{}</dd></div><div><dt>Falhas de envio</dt><dd>{}</dd></div><div><dt>Leituras concluídas</dt><dd>{}</dd></div><div><dt>Falhas de leitura</dt><dd>{}</dd></div></dl><p>Requisições cobrem apenas o endpoint MCP HTTP. Falhas HTTP incluem autenticação e protocolo; falhas das ferramentas incluem negação, validação e erros do Discord. Tempo é medido até a resposta HTTP, não até o fim do corpo, limitado a 10 minutos por requisição. Não contém conteúdo, credenciais ou identificação de usuários e canais.</p>"#,
        summary.http_requests,
        summary.http_failures,
        summary.send_completed,
        summary.send_failures,
        summary.read_completed,
        summary.read_failures
    ))
}

async fn channels(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if !authorized(&app, &headers) {
        return error(StatusCode::FORBIDDEN);
    }
    let names = match store(&app).and_then(|store| store.list()) {
        Ok(value) => value,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let mut rows = String::new();
    for name in names {
        // The store constrains names, but encode them before including in HTML.
        let safe = escape(&name);
        rows.push_str(&format!(
            "<li><a href=\"/admin/channel?name={safe}\">{safe}</a></li>"
        ));
    }
    if rows.is_empty() {
        rows.push_str("<li>Nenhum canal cadastrado.</li>");
    }
    page(format!(
        "<p><a href=\"/admin\">Voltar à visão geral</a></p><h1>Permissões de canais</h1><p>Selecione um canal para consultar e alterar o acesso por usuário.</p><ul>{rows}</ul>"
    ))
}

#[derive(Deserialize)]
struct ChannelQuery {
    name: String,
}

async fn channel(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<ChannelQuery>,
) -> Response {
    if !authorized(&app, &headers) {
        return error(StatusCode::FORBIDDEN);
    }
    let permissions = match store(&app).and_then(|store| store.list_permissions(&query.name)) {
        Ok(value) => value,
        Err(crate::channels::StoreError::NotFound) => return error(StatusCode::NOT_FOUND),
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let name = escape(&query.name);
    let mut rows = String::new();
    for permission in permissions {
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape(&permission.user_id),
            if permission.can_read { "Sim" } else { "Não" },
            if permission.can_send { "Sim" } else { "Não" }
        ));
    }
    if rows.is_empty() {
        rows.push_str("<tr><td colspan=\"3\">Nenhuma permissão concedida.</td></tr>");
    }
    page(format!(
        "<p><a href=\"/admin/channels\">Voltar aos canais</a></p><h1>Canal: {name}</h1><table><thead><tr><th>Usuário Discord</th><th>Leitura</th><th>Envio</th></tr></thead><tbody>{rows}</tbody></table><h2>Alterar permissão</h2><p>Use o ID numérico do usuário Discord. A nova escolha substitui as permissões anteriores.</p><form method=\"post\" action=\"/admin/permissions\"><input type=\"hidden\" name=\"channel\" value=\"{name}\"><label for=\"user_id\">ID do usuário Discord</label><input id=\"user_id\" name=\"user_id\" inputmode=\"numeric\" pattern=\"[0-9]+\" maxlength=\"20\" required><label for=\"permission\">Permissão</label><select id=\"permission\" name=\"permission\"><option value=\"none\">Nenhuma</option><option value=\"read\">Leitura</option><option value=\"send\">Envio</option><option value=\"both\">Leitura e envio</option></select><button>Salvar permissão</button></form>"
    ))
}

#[derive(Deserialize)]
struct PermissionForm {
    user_id: String,
    channel: String,
    permission: String,
}

async fn set_permission(
    State(app): State<Shared>,
    headers: HeaderMap,
    Form(form): Form<PermissionForm>,
) -> Response {
    if !allowed_post(&app, &headers) {
        return error(StatusCode::FORBIDDEN);
    }
    let (read, send) = match form.permission.as_str() {
        "none" => (false, false),
        "read" => (true, false),
        "send" => (false, true),
        "both" => (true, true),
        _ => return error(StatusCode::UNPROCESSABLE_ENTITY),
    };
    let now = match current_timestamp() {
        Ok(value) => value,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let result = store(&app).and_then(|store| {
        store.set_permissions_audited(
            &form.user_id,
            &form.channel,
            read,
            send,
            AuditContext {
                actor: AuditActor::Discord(&app.config.admin_user_id),
                now,
                retention_days: app.config.auth.audit.retention_days(),
            },
        )
    });
    match result {
        Ok(()) => {}
        Err(crate::channels::StoreError::Storage | crate::channels::StoreError::Decryption) => {
            app.config.auth.audit.mark_failure();
            return error(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Err(_) => return error(StatusCode::UNPROCESSABLE_ENTITY),
    }
    let mut target =
        match reqwest::Url::parse(&format!("{}/admin/channel", app.config.public_origin)) {
            Ok(value) => value,
            Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
        };
    target.query_pairs_mut().append_pair("name", &form.channel);
    redirect(target.as_str())
}

async fn logout(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if !allowed_post(&app, &headers) {
        return error(StatusCode::FORBIDDEN);
    }
    if let Some(session) = cookie(&headers, "discord_mcp_admin_session") {
        match app.sessions.lock() {
            Ok(mut sessions) => {
                sessions.remove(&session);
            }
            Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR),
        }
    }
    let mut response = redirect("/admin");
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "discord_mcp_admin_session=; Path=/admin; HttpOnly; SameSite=Lax; Max-Age=0",
        ),
    );
    response
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn headers(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
}

fn page(body: String) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"pt-BR\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Discord MCP · Administração</title><style>body{{font:16px system-ui;background:#101a2d;color:#edf3ff;max-width:860px;margin:2rem auto;padding:0 1rem}}p{{line-height:1.6}}a{{color:#bdd2ff}}label{{display:block;margin:1rem 0 .4rem}}input,select,button{{padding:.7rem;font:inherit}}button{{margin-top:1rem}}td,th{{padding:.5rem;text-align:left}}.table-scroll{{overflow-x:auto}}.metrics{{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,12rem),1fr));gap:1rem}}.metrics div{{padding:1rem;border:1px solid #52637e;border-radius:.4rem;background:#1c2b43}}.metrics dt{{line-height:1.4}}.metrics dd{{margin:.5rem 0 0;font-size:2rem;font-weight:700}}:focus-visible{{outline:3px solid #fbd279}}</style></head><body><main>{body}</main></body></html>"
    );
    let mut response = Html(html).into_response();
    headers(&mut response);
    response
}

fn redirect(target: &str) -> Response {
    let Ok(value) = HeaderValue::from_str(target) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let mut response = StatusCode::SEE_OTHER.into_response();
    response.headers_mut().insert(header::LOCATION, value);
    headers(&mut response);
    response
}

fn error(status: StatusCode) -> Response {
    let mut response = status.into_response();
    headers(&mut response);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::ChannelAction;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn fake_discord() -> (reqwest::Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base =
            reqwest::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let handle = thread::spawn(move || {
            for body in [
                r#"{"access_token":"synthetic-token","token_type":"Bearer"}"#,
                r#"{"id":"123","username":"Admin"}"#,
                r#"{"access_token":"synthetic-token","token_type":"Bearer"}"#,
                r#"{"id":"999","username":"Other"}"#,
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buffer).unwrap();
                    request.extend_from_slice(&buffer[..n]);
                    if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                        let h = String::from_utf8_lossy(&request[..end]);
                        let len = h
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                if key.eq_ignore_ascii_case("content-length") {
                                    value.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        (base, handle)
    }

    #[tokio::test]
    async fn admin_login_is_identity_bound_and_changes_require_session_and_origin() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("admin.db");
        let key = [71; 32];
        let store = ChannelStore::open(&db, &key).unwrap();
        store
            .put_bot("alerts", "123", "synthetic-bot-token")
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (fake_base, fake) = fake_discord();
        let discord = DiscordLoginClient::new(
            "123",
            "synthetic-secret",
            &format!("{origin}/admin/discord/callback"),
        )
        .unwrap()
        .with_test_base(fake_base);
        let config = AdminConfig {
            auth: AuthState {
                database_path: db,
                encryption_key: key,
                resource: format!("{origin}/mcp"),
                metrics: crate::http_auth::MetricsState::default(),
                audit: crate::http_auth::AuditState::default(),
            },
            public_origin: origin.clone(),
            admin_user_id: "123".into(),
            discord,
        };
        let server = tokio::spawn(async move {
            axum::serve(listener, router(config).unwrap())
                .await
                .unwrap()
        });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let channel = format!("{origin}/admin/channel?name=alerts");
        let overview_url = format!("{origin}/admin");
        let metrics_url = format!("{origin}/admin/observability");
        let audit_url = format!("{origin}/admin/audit");
        assert_eq!(
            client.get(&audit_url).send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client.get(&metrics_url).send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let guest = client
            .get(&overview_url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(!guest.contains("Canais cadastrados"));
        assert_eq!(
            client
                .get(format!("{origin}/admin/channels"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client.get(&channel).send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let login = client
            .get(format!("{origin}/admin/login"))
            .send()
            .await
            .unwrap();
        let cookie = login.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let discord_url =
            reqwest::Url::parse(login.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        let state = discord_url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned();
        let mut callback =
            reqwest::Url::parse(&format!("{origin}/admin/discord/callback")).unwrap();
        callback
            .query_pairs_mut()
            .append_pair("state", &state)
            .append_pair("code", "synthetic-code")
            .append_pair("iss", "https://discord.com");
        let wrong = format!("discord_mcp_admin_login={}", "a".repeat(43));
        assert_eq!(
            client
                .get(callback.clone())
                .header(header::COOKIE, wrong)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let response = client
            .get(callback.clone())
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let session = response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        // The browser session is never supplied to the CLI endpoints.
        use sha2::{Digest, Sha256};
        let verifier = [42_u8; 32];
        let challenge = hex::encode(Sha256::digest(verifier));
        let started: rmcp::serde_json::Value = client
            .post(format!("{origin}/v1/cli/requests"))
            .json(&rmcp::serde_json::json!({"user_id":"457", "channel":"alerts", "read":true, "send":false, "challenge":challenge}))
            .send().await.unwrap().json().await.unwrap();
        let id = started["request_id"].as_str().unwrap();
        let execute = || {
            client
                .post(format!("{origin}/v1/cli/execute"))
                .json(&rmcp::serde_json::json!({"request_id":id, "verifier":hex::encode(verifier)}))
        };
        assert_eq!(
            execute().send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let consent = format!("{origin}/admin/cli?request_id={id}");
        let html = client
            .get(&consent)
            .header(header::COOKIE, &session)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(html.contains("457") && html.contains("alerts"));
        assert_eq!(
            client
                .post(format!("{origin}/admin/cli"))
                .form(&[("request_id", id)])
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .post(format!("{origin}/admin/cli"))
                .header(header::COOKIE, &session)
                .header(header::ORIGIN, &origin)
                .form(&[("request_id", id)])
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            client
                .post(format!("{origin}/v1/cli/execute"))
                .json(
                    &rmcp::serde_json::json!({"request_id":id,"verifier":hex::encode([43_u8; 32])})
                )
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            execute().send().await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        assert!(
            store
                .is_permitted("457", "alerts", ChannelAction::Read)
                .unwrap()
        );
        store
            .set_permissions("457", "alerts", false, false)
            .unwrap();
        assert_eq!(
            execute().send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .get(callback)
                .header(header::COOKIE, &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .get(&channel)
                .header(header::COOKIE, &session)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let empty = client
            .get(&metrics_url)
            .header(header::COOKIE, &session)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(empty.contains("Nenhum dado de MCP HTTP registrado"));
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        store
            .record_operational_event(
                now,
                30,
                crate::channels::OperationalEvent::Http {
                    failed: true,
                    response_ms: 12,
                },
            )
            .unwrap();
        store
            .record_operational_event(
                now,
                30,
                crate::channels::OperationalEvent::Send { failed: true },
            )
            .unwrap();
        let metrics = client
            .get(&metrics_url)
            .header(header::COOKIE, &session)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(metrics.contains("<dt>Requisições MCP HTTP concluídas</dt><dd>1</dd>"));
        assert!(metrics.contains("<dt>Respostas HTTP 4xx/5xx</dt><dd>1</dd>"));
        assert!(metrics.contains("<dt>Falhas de envio</dt><dd>1</dd>"));
        assert!(!metrics.contains("synthetic-bot-token"));
        let endpoint = format!("{origin}/admin/permissions");
        let post = || {
            client
                .post(&endpoint)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body("channel=alerts&user_id=456&permission=read")
        };
        assert_eq!(
            post()
                .header(header::ORIGIN, &origin)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            post()
                .header(header::COOKIE, &session)
                .header(header::ORIGIN, "http://evil.invalid")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert!(
            !store
                .is_permitted("456", "alerts", ChannelAction::Read)
                .unwrap()
        );
        assert_eq!(
            post()
                .header(header::COOKIE, &session)
                .header(header::ORIGIN, &origin)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SEE_OTHER
        );
        assert!(
            store
                .is_permitted("456", "alerts", ChannelAction::Read)
                .unwrap()
        );
        assert!(
            !store
                .is_permitted("456", "alerts", ChannelAction::Send)
                .unwrap()
        );
        let audit = client
            .get(format!("{audit_url}?kind=permission_change"))
            .header(header::COOKIE, &session)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(audit.contains("permission_change"));
        assert!(audit.contains("discord: 123"));
        assert!(audit.contains("456"));
        assert!(!audit.contains("synthetic-bot-token"));
        assert!(!audit.contains("<td>admin_login</td>"));
        let response = client
            .get(&overview_url)
            .header(header::COOKIE, &session)
            .send()
            .await
            .unwrap();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let overview = response.text().await.unwrap();
        assert!(overview.contains("<dt>Canais cadastrados</dt><dd>1</dd>"));
        assert!(overview.contains("<dt>Usuários com permissões</dt><dd>1</dd>"));
        assert!(overview.contains("<dt>Concessões de leitura</dt><dd>1</dd>"));
        assert!(overview.contains("<dt>Concessões de envio</dt><dd>0</dd>"));
        assert!(overview.contains("Tokens não expirados não indicam conexões ativas"));
        assert!(!overview.contains("synthetic-bot-token"));
        assert!(overview.contains("/admin/observability"));
        let page = client
            .get(&channel)
            .header(header::COOKIE, &session)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(page.contains("456"));
        let revoke = client
            .post(&endpoint)
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body("channel=alerts&user_id=456&permission=none")
            .send()
            .await
            .unwrap();
        assert_eq!(revoke.status(), StatusCode::SEE_OTHER);
        assert!(
            !store
                .is_permitted("456", "alerts", ChannelAction::Read)
                .unwrap()
        );
        let login = client
            .get(format!("{origin}/admin/login"))
            .send()
            .await
            .unwrap();
        let other_cookie = login.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let url = reqwest::Url::parse(login.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        let state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned();
        let mut callback =
            reqwest::Url::parse(&format!("{origin}/admin/discord/callback")).unwrap();
        callback
            .query_pairs_mut()
            .append_pair("state", &state)
            .append_pair("code", "synthetic-code")
            .append_pair("iss", "https://discord.com");
        let other = client
            .get(callback)
            .header(header::COOKIE, other_cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(other.status(), StatusCode::FORBIDDEN);
        assert!(other.headers().get(header::SET_COOKIE).is_none());
        let logins = store.audit_events(None, Some("admin_login"), 10).unwrap();
        assert_eq!(logins.len(), 2);
        assert_eq!(logins[0].actor_id.as_deref(), Some("999"));
        assert_eq!(logins[0].outcome, "denied");
        assert_eq!(logins[1].actor_id.as_deref(), Some("123"));
        assert_eq!(logins[1].outcome, "success");
        fake.join().unwrap();
        let logout = client
            .post(format!("{origin}/admin/logout"))
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, &origin)
            .send()
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            client
                .get(&channel)
                .header(header::COOKIE, &session)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        server.abort();
    }
}
