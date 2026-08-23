//! Clients in the console: who may reach this store, what they may do, and how to stop them.
//!
//! Until this page, adding a surface meant editing `AUTH_TOKENS` on the box and restarting, or
//! waiting for a client to register itself and then approving it at the consent screen. Neither is
//! something to do from a phone, and the first is the reason a store ends up with one credential
//! doing everything: the cost of a second one is an SSH session.
//!
//! # These are OAuth clients, not bearer tokens
//!
//! A client created here goes in `oauth_client` beside the ones that register themselves, and it
//! completes the OAuth flow to get its own tokens. Nothing here mints a bearer token, and
//! `AUTH_TOKENS` is untouched: a credential the owner put in `.env` keeps working exactly as it
//! did, because auth modes compose rather than exclude.
//!
//! # Creating a client consents to it
//!
//! `set_client_grant` writes `consented_at` alongside the grant, and that is the right reading
//! here. Consent exists so a grant is one the owner approved rather than one a client asked for;
//! an owner filling in this form behind his own password has approved it more directly than a
//! consent screen ever asks. A client that registers itself still goes to that screen, because
//! nobody approved that one yet.
//!
//! # The secret is shown once
//!
//! Confidential clients get a secret at creation and the store keeps only `hash_token` of it, so
//! nothing can show it again. Losing it means issuing another client. That is stricter than
//! `AUTH_TOKENS`, which holds its tokens in plaintext in `.env`, and the difference is deliberate:
//! a console reachable from a browser must not be a place every credential can be read out of.
//!
//! # Revoking is one click and no confirmation
//!
//! Deleting a memory asks first because the owner can miss what is gone. Revoking a client is the
//! opposite: the cost of a mistake is a surface that stops working and says so, and the cost of
//! hesitating is a credential still live while you look for the confirm button.

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde::Deserialize;

use super::{closed, page, redirect, trimmed, Console};
use crate::authserver::pages::escape;
use crate::authserver::session::{OwnerSession, Sessions};
use crate::console::pages::{self, Health, Tab};
use crate::domain::oauth::hash_token;
use crate::domain::policy::NamespaceGrant;
use crate::domain::presets::Preset;
use crate::domain::types::Sensitivity;
use crate::ports::oauth::{ClientGrantUpdate, NewOauthClient, OauthClientRecord};

const NEW_ACTION: &str = "client-new";
const REVOKE_ACTION: &str = "client-revoke";
/// One target for the create form, because it decides nothing that already exists.
const NEW_TARGET: &str = "new";

#[derive(Debug, Default, Deserialize)]
pub struct IndexQuery {
    #[serde(default)]
    done: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct NewClientForm {
    #[serde(default)]
    pub csrf: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub preset: String,
    #[serde(default)]
    pub redirect_uris: String,
    /// Present when the box is ticked, absent otherwise, which is how a browser sends a checkbox.
    #[serde(default)]
    pub confidential: Option<String>,
    /// The advanced view. Empty means the preset decides, which is what most clients want.
    #[serde(default)]
    pub read: String,
    #[serde(default)]
    pub write: String,
    #[serde(default)]
    pub registry_write: Option<String>,
    #[serde(default)]
    pub sealed_capable: Option<String>,
    #[serde(default)]
    pub may_delete: Option<String>,
    #[serde(default)]
    pub may_ingest: Option<String>,
    #[serde(default)]
    pub may_read_history: Option<String>,
    /// Set when the advanced section was open, so an untouched checkbox reads as off rather than as
    /// "the preset decides". Without it every capability a preset grants would be cleared by a form
    /// whose advanced view was never expanded.
    #[serde(default)]
    pub advanced: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct DecisionForm {
    #[serde(default)]
    pub csrf: String,
}

fn ticked(v: &Option<String>) -> bool {
    v.as_deref().is_some_and(|s| !s.is_empty())
}

/// `*@sealed, project:*@open` into grants. A bare glob means a ceiling of open, which is the same
/// reading `AUTH_TOKENS` gives a bare string, so the two surfaces cannot disagree.
pub fn parse_grants(raw: &str) -> Result<Vec<NamespaceGrant>, String> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let t = part.trim();
        if t.is_empty() {
            continue;
        }
        let (ns, max) = match t.split_once('@') {
            Some((ns, m)) => {
                let max = match m.trim() {
                    "open" => Sensitivity::Open,
                    "private" => Sensitivity::Private,
                    "sealed" => Sensitivity::Sealed,
                    other => {
                        return Err(format!(
                            "{other:?} is not a level. Use open, private or sealed."
                        ))
                    }
                };
                (ns.trim(), max)
            }
            None => (t, Sensitivity::Open),
        };
        if ns.is_empty() {
            return Err(format!("{t:?} names no namespace"));
        }
        out.push(NamespaceGrant::new(ns.to_ascii_lowercase(), max));
    }
    Ok(out)
}

/// The grant a submitted form asks for.
///
/// The preset decides unless the advanced view was open, and then every field on it does. A form
/// that merged the two would let a checkbox nobody saw clear a capability the preset granted.
pub fn grant_from(form: &NewClientForm) -> Result<(Preset, ClientGrantUpdate), String> {
    let preset = Preset::parse(form.preset.trim())
        .ok_or_else(|| format!("{:?} is not one of the shapes on this form", form.preset))?;
    let shape = preset.shape();

    if !ticked(&form.advanced) {
        return Ok((
            preset,
            ClientGrantUpdate {
                profile: Some(preset.as_str().to_string()),
                read: shape.read,
                write: shape.write,
                registry_write: shape.registry_write,
                sealed_capable: shape.sealed_capable,
                may_delete: shape.may_delete,
                may_ingest: shape.may_ingest,
                may_read_history: shape.may_read_history,
            },
        ));
    }

    let read = if form.read.trim().is_empty() { shape.read } else { parse_grants(&form.read)? };
    let write = if form.write.trim().is_empty() { shape.write } else { parse_grants(&form.write)? };
    Ok((
        preset,
        ClientGrantUpdate {
            profile: Some(format!("{} (adjusted)", preset.as_str())),
            read,
            write,
            registry_write: ticked(&form.registry_write),
            sealed_capable: ticked(&form.sealed_capable),
            may_delete: ticked(&form.may_delete),
            may_ingest: ticked(&form.may_ingest),
            may_read_history: ticked(&form.may_read_history),
        },
    ))
}

pub async fn index(
    State(app): State<Console>,
    headers: HeaderMap,
    Query(q): Query<IndexQuery>,
) -> Response {
    let session = match app.guard(&headers, "/console/clients") {
        Ok(s) => s,
        Err(response) => return response,
    };
    let Some(sessions) = app.sessions.as_ref() else {
        return closed();
    };
    listing(&app, sessions, &session, None, None, StatusCode::OK, q.done.as_deref()).await
}

pub async fn create(
    State(app): State<Console>,
    headers: HeaderMap,
    Form(form): Form<NewClientForm>,
) -> Response {
    let session = match app.guard(&headers, "/console/clients") {
        Ok(s) => s,
        Err(response) => return response,
    };
    let Some(sessions) = app.sessions.as_ref() else {
        return closed();
    };
    if !sessions.console_csrf_ok(&session, NEW_ACTION, NEW_TARGET, &form.csrf) {
        tracing::warn!("console client refused: the form token did not match");
        return stale();
    }

    let name = trimmed(&form.name).unwrap_or("unnamed client").to_string();
    let (_, grant) = match grant_from(&form) {
        Ok(g) => g,
        Err(e) => {
            return listing(&app, sessions, &session, None, Some(&e), StatusCode::BAD_REQUEST, None)
                .await
        }
    };

    let redirect_uris: Vec<String> = form
        .redirect_uris
        .split(|c| c == ',' || c == '\n')
        .filter_map(|s| trimmed(s).map(str::to_string))
        .collect();
    // The same checks `/oauth/register` runs, count and length included. A URI typed here reaches
    // `/authorize` by the same path a registered one does, so a client the owner issued by hand
    // should not be the one way a fragment, a plain-http host or an oversized list gets stored.
    if !redirect_uris.is_empty() {
        if let Err(e) = crate::domain::oauth::validate_redirect_uris(&redirect_uris) {
            let message = e.client_message().to_string();
            return listing(
                &app,
                sessions,
                &session,
                None,
                Some(&message),
                StatusCode::BAD_REQUEST,
                None,
            )
            .await;
        }
    }

    let client_id = match crate::domain::oauth::random_token(24) {
        Ok(id) => id,
        Err(e) => {
            return listing(
                &app,
                sessions,
                &session,
                None,
                Some(&e.log_message()),
                StatusCode::INTERNAL_SERVER_ERROR,
                None,
            )
            .await
        }
    };
    // A secret only when asked for. A client that runs in a browser cannot keep one, and handing it
    // a secret it will publish is worse than the public client PKCE already secures.
    let secret = if ticked(&form.confidential) {
        match crate::domain::oauth::random_token(32) {
            Ok(s) => Some(s),
            Err(e) => {
                return listing(
                    &app,
                    sessions,
                    &session,
                    None,
                    Some(&e.log_message()),
                    StatusCode::INTERNAL_SERVER_ERROR,
                    None,
                )
                .await
            }
        }
    } else {
        None
    };

    let record = NewOauthClient {
        client_id: client_id.clone(),
        secret_hash: secret.as_deref().map(hash_token),
        client_name: name.clone(),
        redirect_uris,
        grant_types: vec!["authorization_code".into(), "refresh_token".into()],
        software_id: None,
        software_version: None,
        // "manual" is the column's own word for a credential the owner issued rather than one a
        // client asked for, and the consent screen reads it.
        registered_via: "manual".to_string(),
    };
    if let Err(e) = app.state.oauth.register_client(record).await {
        return listing(
            &app,
            sessions,
            &session,
            None,
            Some(&e.log_message()),
            StatusCode::INTERNAL_SERVER_ERROR,
            None,
        )
        .await;
    }
    // The grant, which also records consent. A client registered and left ungranted would sit in
    // the list looking created and reach nothing.
    if let Err(e) = app.state.oauth.set_client_grant(&client_id, grant).await {
        return listing(
            &app,
            sessions,
            &session,
            None,
            Some(&e.log_message()),
            StatusCode::INTERNAL_SERVER_ERROR,
            None,
        )
        .await;
    }

    listing(
        &app,
        sessions,
        &session,
        Some(Issued { client_id, secret, name }),
        None,
        StatusCode::OK,
        None,
    )
    .await
}

pub async fn revoke(
    State(app): State<Console>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<DecisionForm>,
) -> Response {
    let session = match app.guard(&headers, "/console/clients") {
        Ok(s) => s,
        Err(response) => return response,
    };
    let Some(sessions) = app.sessions.as_ref() else {
        return closed();
    };
    if !sessions.console_csrf_ok(&session, REVOKE_ACTION, &id, &form.csrf) {
        tracing::warn!("console client revoke refused: the form token did not match");
        return stale();
    }
    match app.state.oauth.revoke_client(&id).await {
        Ok(true) => redirect("/console/clients?done=revoked"),
        Ok(false) => redirect("/console/clients?done=already-revoked"),
        Err(e) => {
            let health = app.health();
            page(
                StatusCode::INTERNAL_SERVER_ERROR,
                pages::notice("that client was not revoked", &e.client_message(), None, Some(&health)),
            )
        }
    }
}

/// What a creation produced, shown once and never stored anywhere this page can read.
pub struct Issued {
    pub client_id: String,
    pub secret: Option<String>,
    pub name: String,
}

fn stale() -> Response {
    page(
        StatusCode::BAD_REQUEST,
        pages::notice(
            "that form is out of date",
            "The page it came from was drawn for an older sign-in. Open the clients page again and \
             repeat the change.",
            None,
            None,
        ),
    )
}

async fn listing(
    app: &Console,
    sessions: &Sessions,
    session: &OwnerSession,
    issued: Option<Issued>,
    error: Option<&str>,
    status: StatusCode,
    done: Option<&str>,
) -> Response {
    let clients = app.state.oauth.list_clients(true).await.unwrap_or_default();
    let health = app.health();
    let csrf = |action: &str, target: &str| sessions.console_csrf(session, action, target);
    page(status, html(&clients, &health, &csrf, issued.as_ref(), error, done))
}

/// Rendered from a slice and a token maker, so a test draws the page without a server.
pub fn html(
    clients: &[OauthClientRecord],
    health: &Health,
    csrf: &dyn Fn(&str, &str) -> String,
    issued: Option<&Issued>,
    error: Option<&str>,
    done: Option<&str>,
) -> String {
    let mut body = String::from("<main class=\"page\">");

    body.push_str(&format!(
        "<div class=\"pagehead\"><h2>Clients</h2><span class=\"when\">{} registered</span></div>\
<p class=\"lede\">Every surface that reaches this store, and what each may do. A client created \
here is consented to already; one that registered itself waits at the consent screen.</p>",
        clients.len()
    ));

    if let Some(word) = done {
        body.push_str(&format!("<div class=\"note\"><p>{}</p></div>", escape(word)));
    }
    if let Some(e) = error {
        body.push_str(&format!("<div class=\"note\"><p>{}</p></div>", escape(e)));
    }

    // The one time the secret is readable. Above the list, because scrolling past it is how it gets
    // lost.
    if let Some(i) = issued {
        body.push_str(&format!(
            "<div class=\"note\"><div class=\"big2\">{name} is ready</div>\
<p>Client id</p><p><code>{id}</code></p>",
            name = escape(&i.name),
            id = escape(&i.client_id)
        ));
        match &i.secret {
            Some(s) => body.push_str(&format!(
                "<p>Client secret</p><p><code>{}</code></p>\
<p>Copy it now. The store keeps only a hash of it, so nothing here can show it again. Losing it \
means issuing another client.</p>",
                escape(s)
            )),
            None => body.push_str(
                "<p>A public client, so there is no secret. It proves itself with PKCE, which is \
                 what a client that cannot keep a secret should use.</p>",
            ),
        }
        body.push_str("</div>");
    }

    body.push_str(&client_table(clients, csrf));
    body.push_str(&new_form(csrf));
    body.push_str("</main>");

    pages::shell("lumberroom: clients", Tab::Clients, Some(health), &body)
}

fn client_table(clients: &[OauthClientRecord], csrf: &dyn Fn(&str, &str) -> String) -> String {
    if clients.is_empty() {
        return "<div class=\"none\"><p class=\"big\">Nothing reaches this store yet.</p>\
<p>Create a client below, or point a client at this server and approve it when it asks.</p></div>"
            .to_string();
    }
    // One `.reg` per row with three cells, which is the shape the registry page established and the
    // shape the grid expects. Wrapping every row in one `.reg` and emitting two cells put every
    // second row one column out of step, so nothing lined up with anything.
    let mut out = String::new();
    for c in clients {
        let revoked = c.revoked_at.is_some();
        let state = if revoked {
            "<span class=\"kicker\">revoked</span>"
        } else if c.consented_at.is_some() {
            "<span class=\"kicker\">consented</span>"
        } else {
            "<span class=\"kicker\">awaiting consent</span>"
        };
        let control = if revoked {
            String::new()
        } else {
            format!(
                "<form method=\"post\" action=\"/console/clients/{id}/revoke\">\
<input type=\"hidden\" name=\"csrf\" value=\"{token}\">\
<button type=\"submit\">Revoke</button></form>",
                id = escape(&c.client_id),
                token = escape(&csrf(REVOKE_ACTION, &c.client_id))
            )
        };
        out.push_str(&format!(
            "<div class=\"reg\"><span class=\"rk\">{name}<small>{via}</small></span>\
<span class=\"rv\"><code class=\"mono\">{id}</code><span class=\"why\">{grant}</span></span>\
<span class=\"rp\">{state}{control}</span></div>",
            name = escape(&c.client_name),
            via = escape(&c.registered_via),
            id = escape(&c.client_id),
            grant = escape(&grant_line(c)),
            state = state,
            control = control,
        ));
    }
    out
}

/// The grant in one line a person can scan, rather than two JSON blobs.
pub fn grant_line(c: &OauthClientRecord) -> String {
    let side = |label: &str, g: &[NamespaceGrant]| -> String {
        if g.is_empty() {
            return format!("{label} nothing");
        }
        let names: Vec<String> =
            g.iter().map(|n| format!("{}@{}", n.namespace, n.max.as_str())).collect();
        format!("{label} {}", names.join(" "))
    };
    let mut caps: Vec<&str> = Vec::new();
    if c.registry_write {
        caps.push("registryWrite");
    }
    if c.sealed_capable {
        caps.push("sealedCapable");
    }
    if c.may_delete {
        caps.push("mayDelete");
    }
    if c.may_ingest {
        caps.push("mayIngest");
    }
    if c.may_read_history {
        caps.push("mayReadHistory");
    }
    let caps = if caps.is_empty() { "no capabilities".to_string() } else { caps.join(", ") };
    format!("{}, {}, {caps}", side("reads", &c.read), side("writes", &c.write))
}

fn new_form(csrf: &dyn Fn(&str, &str) -> String) -> String {
    let mut shapes = String::new();
    for (i, p) in Preset::ALL.into_iter().enumerate() {
        shapes.push_str(&format!(
            "<label class=\"f\"><input type=\"radio\" name=\"preset\" value=\"{value}\"{checked}> \
<b>{title}</b><span class=\"hint\">{detail}</span></label>",
            value = p.as_str(),
            checked = if i == 1 { " checked" } else { "" },
            title = escape(p.title()),
            detail = escape(p.detail()),
        ));
    }

    format!(
        "<div class=\"wr\"><h3>New client</h3>\
<form method=\"post\" action=\"/console/clients/new\">\
<input type=\"hidden\" name=\"csrf\" value=\"{token}\">\
<label class=\"f\">Name<input type=\"text\" name=\"name\" placeholder=\"claude-desktop\" \
autocomplete=\"off\"></label>\
<span class=\"hint\">What you will call it in this list. It is a label and never an identity.</span>\
{shapes}\
<label class=\"f\">Redirect URIs<input type=\"text\" name=\"redirect_uris\" \
placeholder=\"https://claude.ai/api/mcp/auth_callback\" autocomplete=\"off\"></label>\
<span class=\"hint\">Comma separated. Where the client is sent back to after it signs in. \
Matched exactly, so a trailing slash is a different address.</span>\
<label class=\"f\"><input type=\"checkbox\" name=\"confidential\" value=\"1\"> \
Issue a client secret<span class=\"hint\">For a client that runs on a server and can keep one. A \
client running in a browser cannot, and should stay public: PKCE is what binds its exchange. The \
secret is shown once and stored as a hash.</span></label>\
<details><summary>Adjust namespaces and capabilities</summary>\
<label class=\"f\"><input type=\"checkbox\" name=\"advanced\" value=\"1\"> \
Use the fields below instead of the shape above<span class=\"hint\">Leave this unticked and the \
shape decides everything. Ticked, every field here applies, including the boxes you did not tick.\
</span></label>\
<label class=\"f\">Reads<input type=\"text\" name=\"read\" placeholder=\"*@sealed\" \
autocomplete=\"off\"></label>\
<label class=\"f\">Writes<input type=\"text\" name=\"write\" placeholder=\"*@open, project:*@private\" \
autocomplete=\"off\"></label>\
<span class=\"hint\">Comma separated globs, each with a ceiling after an @. A bare glob means \
open, the same reading AUTH_TOKENS gives a bare string. Empty means the shape decides that side.\
</span>\
<label class=\"f\"><input type=\"checkbox\" name=\"registry_write\" value=\"1\"> registryWrite\
<span class=\"hint\">Writes the registry and records aliases. The registry holds credential \
locations.</span></label>\
<label class=\"f\"><input type=\"checkbox\" name=\"sealed_capable\" value=\"1\"> sealedCapable\
<span class=\"hint\">Asserts the client decrypts locally. Without it a sealed row is ciphertext \
however high the ceiling.</span></label>\
<label class=\"f\"><input type=\"checkbox\" name=\"may_ingest\" value=\"1\"> mayIngest\
<span class=\"hint\">Fills the proposal queue and reaches the cleanup routes.</span></label>\
<label class=\"f\"><input type=\"checkbox\" name=\"may_read_history\" value=\"1\"> mayReadHistory\
<span class=\"hint\">Reads retired facts. A retired fact can be more revealing than the one that \
replaced it.</span></label>\
<label class=\"f\"><input type=\"checkbox\" name=\"may_delete\" value=\"1\"> mayDelete\
<span class=\"hint\">Deletes memories. No shape grants this, and a client that can silently remove \
a memory is a worse failure than one that hoards them.</span></label>\
</details>\
<div class=\"send\"><button type=\"submit\" class=\"go\">Create it</button>\
<a href=\"/console/reading\">Back to reading</a></div>\
</form></div>",
        token = escape(&csrf(NEW_ACTION, NEW_TARGET)),
        shapes = shapes,
    )
}
