//! The cleanup queue in the console: what a pass thinks the store has accumulated, and the two
//! answers the owner gives it.
//!
//! A pass reads the store as a whole, finds duplicates, paraphrases, contradictions and rows
//! nothing has ever read, and writes a proposal. It retires nothing. This page is where the owner
//! reads one and decides it, and both controls call the `services::cleanup` functions
//! `lumberroom cleanup apply` and `lumberroom cleanup reject` call. Decision 0006 makes that argument
//! for the ingest queue and it carries here unchanged: the handler holds no rule of its own, so a
//! rule added on this surface rather than in the service cannot exist.
//!
//! # A contradiction carries no Apply
//!
//! `CleanupKind::has_keep` is false for it and `deletes` is false too, so `services::cleanup::apply`
//! refuses it with `ApplyRefusal::NothingToApply`. A page that drew the button anyway would offer a
//! control whose only outcome is a refusal page. The reason is worth printing rather than hiding:
//! which of two conflicting facts holds is the owner's call, and a pass that picked the winner
//! would be writing the fact rather than reporting the conflict.
//!
//! # The one capability, and only for the kind that needs it
//!
//! Console handlers do not go through `ingest_ctx`, so nothing upstream of this file checks a
//! capability. What decides whether the Apply button works is the principal the handler builds, and
//! `owner_approver` alone is not enough: a `stale` proposal deletes its rows through
//! `services::review::delete`, which refuses a principal with `may_delete` false. So `operator`
//! adds that flag for `Stale` and for no other kind, the shape `aliases::alias_operator` uses. A
//! blanket grant would reopen the blast radius decision 0006 settled, to buy nothing: every other
//! kind supersedes.
//!
//! # Why the page carries its own stylesheet
//!
//! `pages::STYLE` and `pages::shell` are private to that module and this file does not own it, so
//! the chrome is spelled again here, the way `aliases.rs` does. Every selector is prefixed `cl-`,
//! which makes merging this constant into `STYLE` later a paste rather than a collision hunt.

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde::Deserialize;

use super::{closed, failed, page, redirect, refusal, trimmed, Console};
use crate::authserver::pages::escape;
use crate::console::pages::{self, Health};
use crate::domain::cleanup::CleanupKind;
use crate::domain::policy::NamespaceGrant;
use crate::domain::types::{Principal, Sensitivity};
use crate::ports::cleanup::{Member, Proposal};
use crate::services::cleanup as service;

/// The two actions a token on this page is minted for. A token that rejects a finding must not be
/// spendable on the form that applies one, and the action is signed into it.
const APPLY_ACTION: &str = "cleanup-apply";
const REJECT_ACTION: &str = "cleanup-reject";
const RESOLVE_ACTION: &str = "cleanup-resolve";

/// How many proposals the page asks for. The service clamps at 200, and a queue longer than this
/// is one for `lumberroom cleanup list`, which the page says.
const LIMIT: i64 = 100;

// ---- the principal ----

/// The console reader, plus the write grant every apply needs, plus delete for the one kind that
/// deletes.
///
/// The write grant matches `owner_approver`'s and for the reason that one gives: it is the ceiling
/// the owner's own CLI credential already carries, and a narrower one would refuse a row
/// `lumberroom cleanup apply` takes and read as a broken button.
///
/// `may_delete` follows the kind. `Stale` is the only kind whose members have nothing to supersede
/// into, so applying it calls `services::review::delete`, which refuses without the flag. Every
/// other kind retires into a survivor and the flag stays false, which keeps the delete path
/// unreachable from a page showing a paraphrase.
fn operator(reader: Principal, kind: CleanupKind) -> Principal {
    Principal {
        write: vec![NamespaceGrant::new("*", Sensitivity::Sealed)],
        may_delete: kind.deletes(),
        ..reader
    }
}

/// Whether this kind has anything to apply, mirroring `services::cleanup::apply`'s own refusal.
///
/// Spelled here so the button and the handler agree by reading the same two predicates. A page that
/// decided this some other way would draw a control the service refuses, which reads as a broken
/// button and sends the owner to the terminal.
fn applicable(kind: CleanupKind) -> bool {
    kind.has_keep() || kind.deletes()
}

// ---- the routes ----

#[derive(Debug, Deserialize)]
pub struct IndexQuery {
    #[serde(default)]
    pub done: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ResolveForm {
    #[serde(default)]
    pub csrf: String,
    /// Which of the finding's rows holds. Checked against the proposal's own members by the
    /// service, so a hand-edited form cannot name a row the finding was never about.
    #[serde(default)]
    pub keep_id: String,
}

#[derive(Deserialize)]
pub struct DecisionForm {
    #[serde(default)]
    pub csrf: String,
    /// Only the reject form carries one, and it is optional there. A refused finding comes back
    /// next hour under the same cluster key, so the note is the only record of why it was refused.
    #[serde(default)]
    pub reason: String,
}

/// The queue: every proposal this reader may see, newest first, grouped by what was decided.
pub async fn index(
    State(app): State<Console>,
    headers: HeaderMap,
    Query(q): Query<IndexQuery>,
) -> Response {
    let session = match app.guard(&headers, "/console/cleanup") {
        Ok(s) => s,
        Err(response) => return response,
    };
    let Some(sessions) = app.sessions.as_ref() else {
        return closed();
    };

    let proposals =
        match service::list(&app.ctx(), app.state.cleanup.as_ref(), None, LIMIT).await {
            Ok(rows) => rows,
            Err(e) => return failed(&app, "the cleanup queue did not load", &e),
        };

    let csrf = |action: &str, id: &str| sessions.console_csrf(&session, action, id);
    page(
        StatusCode::OK,
        listing_html(&proposals, &app.health(), &csrf, q.done.as_deref()),
    )
}

/// Carry out one proposal. Token first, then the kind, then the service.
///
/// The proposal is read before the principal is built, because the kind decides which capability
/// the act needs. `services::cleanup::apply` reads it again and holds every rule: the state check,
/// the member checks that refuse a cluster the store has moved under, and the supersede or delete
/// call itself.
pub async fn apply(
    State(app): State<Console>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<DecisionForm>,
) -> Response {
    if let Err(response) = decided(&app, &headers, APPLY_ACTION, &id, &form.csrf) {
        return response;
    }

    let found = match service::get(&app.ctx(), app.state.cleanup.as_ref(), &id).await {
        Ok(p) => p,
        Err(e) => return failed(&app, "the proposal did not load", &e),
    };
    let Some(proposal) = found else {
        return missing();
    };

    let ctx = app.ctx_with(operator(app.owner_reader(), proposal.kind));
    match service::apply(&ctx, app.state.cleanup.as_ref(), &id).await {
        Ok(_) => done("applied"),
        Err(e) => refusal(&app, "that proposal was not applied", &e),
    }
}

/// Settle a contradiction by naming which of its rows holds.
pub async fn resolve(
    State(app): State<Console>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<ResolveForm>,
) -> Response {
    if let Err(response) = decided(&app, &headers, RESOLVE_ACTION, &id, &form.csrf) {
        return response;
    }

    let found = match service::get(&app.ctx(), app.state.cleanup.as_ref(), &id).await {
        Ok(p) => p,
        Err(e) => return failed(&app, "the proposal did not load", &e),
    };
    let Some(proposal) = found else {
        return missing();
    };

    let ctx = app.ctx_with(operator(app.owner_reader(), proposal.kind));
    match service::resolve(&ctx, app.state.cleanup.as_ref(), &id, &form.keep_id).await {
        Ok(_) => done("resolved"),
        Err(e) => refusal(&app, "that contradiction was not resolved", &e),
    }
}

/// Refuse one proposal. The cluster stays out of the queue afterwards, which is what makes an
/// hourly pass safe to run hourly.
pub async fn reject(
    State(app): State<Console>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<DecisionForm>,
) -> Response {
    if let Err(response) = decided(&app, &headers, REJECT_ACTION, &id, &form.csrf) {
        return response;
    }
    match service::reject(
        &app.ctx(),
        app.state.cleanup.as_ref(),
        &id,
        trimmed(&form.reason),
    )
    .await
    {
        Ok(()) => done("rejected"),
        Err(e) => refusal(&app, "that proposal was not rejected", &e),
    }
}

/// The token check, before anything reaches the store.
///
/// A stale form answers with a page rather than a redirect: a bounce back to the queue would look
/// like the decision went through.
fn decided(
    app: &Console,
    headers: &HeaderMap,
    action: &str,
    id: &str,
    presented: &str,
) -> Result<(), Response> {
    let session = app.guard(headers, "/console/cleanup")?;
    let Some(sessions) = app.sessions.as_ref() else {
        return Err(closed());
    };
    if sessions.console_csrf_ok(&session, action, id, presented) {
        return Ok(());
    }
    tracing::warn!(action, "console cleanup decision refused: the form token did not match");
    Err(page(
        StatusCode::FORBIDDEN,
        pages::notice(
            "that form went stale",
            "The token on it was minted for another session, another finding or another act, so \
             nothing was changed. Reload the cleanup queue and decide again.",
            None,
            None,
        ),
    ))
}

fn missing() -> Response {
    page(
        StatusCode::NOT_FOUND,
        pages::notice(
            "no such finding",
            "Nothing in the cleanup queue carries the id in that address. A pass may have closed \
             it because the store already answered it.",
            None,
            None,
        ),
    )
}

/// Back to the queue with one word saying what happened. 303, so a refresh does not decide twice.
fn done(outcome: &str) -> Response {
    redirect(&format!("/console/cleanup?done={outcome}"))
}

// ---- the page ----

/// The whole page: the findings waiting, then what was already decided.
///
/// `csrf` arrives as a maker so the session type stays out of the rendering and a test can draw the
/// page without one, the shape the ingest queue and the aliases page both take.
pub fn listing_html(
    proposals: &[Proposal],
    health: &Health,
    csrf: &dyn Fn(&str, &str) -> String,
    done: Option<&str>,
) -> String {
    let waiting: Vec<&Proposal> = of_state(proposals, "proposed");
    let applied = of_state(proposals, "applied");
    let rejected = of_state(proposals, "rejected");
    let closed = of_state(proposals, "obsolete");

    let body = if proposals.is_empty() {
        "<div class=\"cl-none\"><div class=\"big\">Nothing has piled up.</div>\
<p>A pass reads the store as a whole and queues what it finds: two rows saying one thing, two rows \
that cannot both hold, and rows nothing has ever read. It retires none of them. Run one and what \
it finds lands here.</p><code>lumberroom cleanup run</code></div>"
            .to_string()
    } else {
        let mut out = String::new();
        out.push_str(&section(
            "Waiting",
            "undecided. Apply carries the finding out through the same path the command line uses; \
             reject keeps this cluster out of the queue for good.",
            &waiting,
            true,
            csrf,
        ));
        out.push_str(&section("Applied", "already carried out.", &applied, false, csrf));
        out.push_str(&section(
            "Rejected",
            "the owner refused these, and the pass will not raise them again.",
            &rejected,
            false,
            csrf,
        ));
        out.push_str(&section(
            "Closed",
            "the store answered these before anybody pressed a button, so there is nothing left to \
             decide.",
            &closed,
            false,
            csrf,
        ));
        out
    };

    frame(
        "lumberroom: cleanup",
        health,
        &format!(
            "<main class=\"cl-page\">\
<div class=\"cl-head\"><h2>Cleanup</h2><div class=\"when\">{count}</div></div>\
<p class=\"cl-lede\">A store that only grows stops being readable. What a pass proposes here is a \
reading of the whole store, and every finding names the rows it is about and what produced it. \
Nothing is retired until you say so.</p>{done}\
<div class=\"cl-bulk\"><p>The command line still clears a queue in bulk: \
<code>lumberroom cleanup list</code> and <code>lumberroom cleanup apply &lt;id&gt;</code>. This page \
draws the newest {limit}.</p></div>{body}</main>",
            count = escape(&counted(waiting.len(), proposals.len())),
            done = done_note(done),
            limit = LIMIT,
            body = body,
        ),
    )
}

fn of_state<'a>(proposals: &'a [Proposal], state: &str) -> Vec<&'a Proposal> {
    proposals.iter().filter(|p| p.state == state).collect()
}

/// The count in the head. A queue with nothing waiting says that first, because it is the answer to
/// the question the reader opened the page with.
fn counted(waiting: usize, total: usize) -> String {
    if total == 0 {
        return "nothing found yet".to_string();
    }
    if waiting == 0 {
        return format!("nothing waiting, {total} decided");
    }
    format!(
        "{waiting} {finding} waiting of {total}",
        finding = plural(waiting, "finding", "findings")
    )
}

fn section(
    title: &str,
    lede: &str,
    rows: &[&Proposal],
    controls: bool,
    csrf: &dyn Fn(&str, &str) -> String,
) -> String {
    if rows.is_empty() {
        return String::new();
    }
    format!(
        "<section class=\"cl-sec\"><h3>{title} <span>{n}</span></h3><p class=\"cl-secline\">{lede}</p>{rows}</section>",
        title = escape(title),
        n = rows.len(),
        lede = escape(lede),
        rows = rows.iter().map(|p| proposal_html(p, controls, csrf)).collect::<String>(),
    )
}

/// One finding: what it claims, what produced it, every row it is about, and the controls.
fn proposal_html(p: &Proposal, controls: bool, csrf: &dyn Fn(&str, &str) -> String) -> String {
    let members: String = p.members.iter().map(member_html).collect();
    let acts = if controls { controls_html(p, csrf) } else { decided_html(p) };
    format!(
        "<article class=\"cl-item\"><div class=\"cl-meta\">\
<span class=\"cl-kind\">{kind}</span><span class=\"cl-sim\">{sim}</span>\
<span class=\"cl-ns\">{namespace}</span><span class=\"cl-by\">via {by}</span></div>\
<p class=\"cl-why\">{why}</p><div class=\"cl-mems\">{members}</div>{acts}</article>",
        kind = escape(p.kind.as_str()),
        sim = escape(&similarity(p.similarity)),
        namespace = escape(&p.namespace),
        by = escape(&p.produced_by),
        why = escape(&p.rationale),
        members = members,
        acts = acts,
    )
}

/// One row in the cluster, its disposition, and whether the store still holds what the pass read.
///
/// The text printed is `seen_content`, which is what the pass was looking at when it wrote the
/// finding. `current_content` decides the mark beside it and nothing else: a row that has changed
/// since prints both facts, that the pass read one thing and that the store now says another.
fn member_html(m: &Member) -> String {
    let mark = match member_mark(m) {
        Some(word) => format!("<span class=\"cl-moved\">{}</span>", escape(word)),
        None => String::new(),
    };
    format!(
        "<div class=\"cl-mem\"><span class=\"cl-disp\">{disposition}</span>\
<a class=\"cl-id\" href=\"/console/fact/{id}\">{id}</a>{mark}\
<p class=\"cl-text\">{text}</p></div>",
        disposition = escape(m.disposition.as_str()),
        id = escape(&m.memory_id),
        mark = mark,
        text = escape(&m.seen_content),
    )
}

/// Enough of a fact to tell two rows apart on a button, and no more.
fn preview(text: &str) -> String {
    let t = text.trim();
    match t.char_indices().nth(90) {
        Some((i, _)) => format!("{}...", &t[..i]),
        None => t.to_string(),
    }
}

/// Whether the store has moved under this member, in the words `lumberroom cleanup show` uses.
///
/// The order is load-bearing and it is the CLI's. A row that is gone is gone whatever else was true
/// of it, and a row already retired says so rather than reporting an edit, because the supersession
/// is the more useful half. Every one of these three refuses the apply downstream, so the mark is
/// the page saying in advance what the button would say afterwards.
fn member_mark(m: &Member) -> Option<&'static str> {
    match (m.current_content.as_deref(), m.superseded_by.as_deref()) {
        (None, _) => Some("GONE"),
        (_, Some(_)) => Some("ALREADY RETIRED"),
        (Some(now), None) if now != m.seen_content => Some("EDITED SINCE"),
        _ => None,
    }
}

/// The controls under a waiting finding.
///
/// A contradiction gets Reject and a sentence instead of Apply. `services::cleanup::apply` refuses
/// that kind, so drawing the button would offer a control with one outcome, and the outcome is a
/// refusal page that explains what this sentence explains better.
fn controls_html(p: &Proposal, csrf: &dyn Fn(&str, &str) -> String) -> String {
    let apply = if applicable(p.kind) {
        format!(
            "<form method=\"post\" action=\"/console/cleanup/{id}/apply\">\
<input type=\"hidden\" name=\"csrf\" value=\"{token}\">\
<button type=\"submit\" class=\"go\">{label}</button></form>",
            id = escape(&p.id),
            token = escape(&csrf(APPLY_ACTION, &p.id)),
            label = if p.kind.deletes() { "Apply, deleting" } else { "Apply" },
        )
    } else {
        String::new()
    };
    // A contradiction gets one button per row instead of an Apply. Naming which fact holds is the
    // owner's call, which is why the pass leaves it, and this is where he makes it.
    //
    // The page used to tell him to open a row and supersede it from the fact page. That advice was
    // wrong: the fact page's Replace composes a NEW memory superseding the one being viewed, so on
    // a contradiction it leaves three rows, the new one, the row it retired, and the other original
    // still live. Superseding one existing row into another is a different operation and this is
    // the only surface that offers it.
    let note = if applicable(p.kind) {
        String::new()
    } else {
        let keeps: String = p
            .members
            .iter()
            .filter(|m| member_mark(m).is_none())
            .map(|m| {
                format!(
                    "<form method=\"post\" action=\"/console/cleanup/{id}/resolve\">\
<input type=\"hidden\" name=\"csrf\" value=\"{token}\">\
<input type=\"hidden\" name=\"keep_id\" value=\"{keep}\">\
<button type=\"submit\">Keep this one</button>\
<span class=\"cl-keep-text\">{text}</span></form>",
                    id = escape(&p.id),
                    token = escape(&csrf(RESOLVE_ACTION, &p.id)),
                    keep = escape(&m.memory_id),
                    text = escape(&preview(&m.seen_content)),
                )
            })
            .collect();
        format!(
            "<p class=\"cl-note\">A contradiction names no survivor: which of two conflicting \
facts holds is yours to call, and a pass that picked the winner would be writing the fact rather \
than reporting the conflict. Keeping one retires the other into it, and the retired text stays \
readable through its history.</p><div class=\"cl-keeps\">{keeps}</div>"
        )
    };
    format!(
        // One placement rule, and this is where it was broken: the reason field sat between the
        // primary and the refusal, so a destructive control was the far side of a text box. The
        // acts the screen exists for come first, then a rule, then the refusal with its reason.
        "{note}<div class=\"cl-acts\"><div class=\"cl-main\">{apply}</div>\
<form class=\"cl-quiet\" method=\"post\" action=\"/console/cleanup/{id}/reject\">\
<input type=\"hidden\" name=\"csrf\" value=\"{token}\">\
<input type=\"text\" name=\"reason\" placeholder=\"why not (optional)\" autocomplete=\"off\">\
<button type=\"submit\">Reject</button></form></div>",
        note = note,
        apply = apply,
        id = escape(&p.id),
        token = escape(&csrf(REJECT_ACTION, &p.id)),
    )
}

/// What a decided finding says instead of controls. The reason is printed because a rejection with
/// no note is a decision nobody can evaluate a month later.
fn decided_html(p: &Proposal) -> String {
    let when = match &p.decided_at {
        Some(t) => t.format("%Y-%m-%d").to_string(),
        None => String::new(),
    };
    let reason = match p.reason.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => format!(" &middot; {}", escape(r)),
        None => String::new(),
    };
    format!(
        "<p class=\"cl-state\">{state} {when}{reason}</p>",
        state = escape(&p.state),
        when = escape(&when),
        reason = reason,
    )
}

/// The line the page prints after a decision.
///
/// A closed list. Each word came from a redirect this file wrote, so one it does not recognise
/// prints nothing rather than reaching the page as text.
fn done_note(done: Option<&str>) -> String {
    let line = match done.unwrap_or_default() {
        "applied" => "Applied. The rows it named moved, and a retired one is still readable through \
                      its history.",
        "rejected" => "Rejected. The pass will not raise that cluster again.",
        _ => return String::new(),
    };
    format!("<p class=\"cl-done\">{}</p>", escape(line))
}

/// The cosine that grouped a cluster, or a word saying a model grouped it instead.
fn similarity(value: Option<f64>) -> String {
    match value {
        Some(s) => format!("{s:.3}"),
        None => "no score".to_string(),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { one.to_string() } else { many.to_string() }
}

/// The document, the chrome and the health line.
fn frame(title: &str, health: &Health, body: &str) -> String {
    let bad = !health.key_verified || health.degraded_embedder;
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<meta name=\"robots\" content=\"noindex,nofollow\">{favicon}\
<title>{title}</title><style>{STYLE}</style></head>\n<body><div class=\"cl-doc\">\
<header class=\"cl-top\">{brand}\
<nav class=\"cl-nav\">{nav}</nav>\
<div class=\"cl-health{badclass}\">{line}</div></header>{body}</div></body></html>\n",
        title = escape(title),
        favicon = super::pages::FAVICON,
        brand = super::pages::BRAND,
        nav = nav(),
        badclass = if bad { " bad" } else { "" },
        line = health_line(health),
        body = body,
    )
}

fn nav() -> String {
    super::pages::nav(super::pages::Tab::Cleanup)
}

/// What the server can and cannot do right now, in the words the rest of the console uses.
fn health_line(health: &Health) -> String {
    let key = if !health.keys_configured {
        "key <b>not configured</b>"
    } else if health.key_verified {
        "key <b>verified</b>"
    } else {
        "key <b>does not match</b>"
    };
    let embedder = if health.degraded_embedder {
        format!("embedder <b>{} fallback</b>", escape(&health.embedder))
    } else {
        format!("embedder {}", escape(&health.embedder))
    };
    format!("{key} &middot; {embedder}")
}

/// This page's stylesheet, scoped to `cl-` so it can be appended to `pages::STYLE` unchanged.
///
/// The custom properties sit on `.cl-doc` rather than `:root` for the same reason `aliases.rs` puts
/// its own on `.al-doc`: a merged copy must not fight the block that file already declares.
const STYLE: &str = "\
html,body{background:#faf7f1;margin:0}
.cl-doc{--paper:#faf7f1;--paper-2:#f4efe5;--paper-3:#efe8da;--ink:#211c17;--ink-2:#4a423a;
 --ink-3:#6b6258;--rule:#ddd4c4;--rule-2:#c3b7a2;--rule-3:#9a8d78;--pencil:#a3341f;
 --pencil-bg:#fbeee9;--blue:#1c4f8f;--green:#2c5f34;
 --serif:\"Iowan Old Style\",\"Palatino Linotype\",Palatino,\"Book Antiqua\",Georgia,\"Times New Roman\",serif;
 --sans:system-ui,-apple-system,\"Segoe UI\",Roboto,\"Helvetica Neue\",Arial,sans-serif;
 --mono:ui-monospace,\"SF Mono\",Menlo,Consolas,\"Liberation Mono\",monospace;
 background:var(--paper);color:var(--ink);font:400 15px/1.5 var(--sans)}
.cl-doc *{box-sizing:border-box;margin:0;padding:0}
.cl-doc :focus-visible{outline:2px solid var(--blue);outline-offset:2px}
.cl-doc a{color:inherit}
.cl-top{display:flex;align-items:center;gap:16px;flex-wrap:wrap;padding:8px 24px;
 border-bottom:1px solid var(--rule-2);background:var(--paper-2)}
.brand{display:flex;align-items:center;gap:8px;text-decoration:none;color:inherit}
.brand img{display:block;width:24px;height:24px}
.brand span{font:600 15px/1 var(--serif)}
.brand em{font-style:normal;font:400 11px/1 var(--sans);color:var(--ink-3);
 text-transform:uppercase;letter-spacing:.1em;margin-left:8px}
.cl-nav{display:flex;flex:1;flex-wrap:wrap}
.cl-nav a{font:500 13px/1 var(--sans);color:var(--ink-2);text-decoration:none;padding:8px 12px;
 border-bottom:2px solid transparent}
.cl-nav a.on{color:var(--ink);font-weight:700;border-bottom-color:var(--ink)}
.cl-health{font:400 13px/1.4 var(--mono);color:var(--ink-3)}
.cl-health b{color:var(--green);font-weight:600}
.cl-health.bad b{color:var(--pencil)}
.cl-page{padding:16px 24px 64px;max-width:1000px}
.cl-head{display:flex;align-items:flex-end;gap:16px;flex-wrap:wrap;padding-bottom:8px;
 border-bottom:2px solid var(--rule-2)}
.cl-head h2{font:600 22px/1.15 var(--serif);letter-spacing:-.005em}
.cl-head .when{font:400 13px/1.3 var(--sans);color:var(--ink-2)}
.cl-lede{font:400 18px/1.6 var(--serif);color:var(--ink-2);max-width:64ch;margin-top:12px}
.cl-bulk{margin-top:12px;max-width:64ch}
.cl-bulk p{font:400 13px/1.55 var(--sans);color:var(--ink-3)}
.cl-bulk code,.cl-none code{font:400 13px/1.5 var(--mono);background:var(--paper-3);
 padding:0px 4px}
.cl-none{max-width:64ch;margin-top:24px}
.cl-none .big{font:400 27px/1.35 var(--serif);max-width:32ch}
.cl-none p{font:400 18px/1.6 var(--serif);color:var(--ink-2);margin:12px 0}
.cl-sec{margin-top:32px}
.cl-sec h3{font:700 11px/1.3 var(--sans);text-transform:uppercase;letter-spacing:.1em;
 color:var(--ink-3);padding-bottom:4px;border-bottom:1px solid var(--rule-2)}
.cl-sec h3 span{color:var(--ink-2)}
.cl-secline{font:400 13px/1.55 var(--sans);color:var(--ink-3);margin-top:8px;max-width:64ch}
.cl-item{padding:12px 0;border-bottom:1px solid var(--rule)}
.cl-meta{display:flex;gap:12px;flex-wrap:wrap;align-items:baseline}
.cl-kind{font:700 11px/1.3 var(--sans);text-transform:uppercase;letter-spacing:.09em;
 color:var(--pencil)}
.cl-sim{font:400 13px/1.4 var(--mono);color:var(--ink-2);font-variant-numeric:tabular-nums}
.cl-ns{font:400 13px/1.4 var(--mono);color:var(--ink-2)}
.cl-by{font:400 11px/1.5 var(--mono);color:var(--ink-3)}
.cl-why{font:400 15px/1.55 var(--serif);color:var(--ink);margin-top:8px;max-width:70ch;
 overflow-wrap:anywhere}
.cl-mems{margin-top:8px;border-left:2px solid var(--rule-2);padding-left:12px}
.cl-mem{padding:4px 0}
.cl-disp{font:700 11px/1.3 var(--sans);text-transform:uppercase;letter-spacing:.09em;
 color:var(--ink-3);margin-right:8px}
.cl-id{font:400 11px/1.4 var(--mono);color:var(--blue)}
.cl-moved{font:700 11px/1.3 var(--sans);letter-spacing:.08em;color:var(--pencil);
 background:var(--pencil-bg);padding:0px 4px;margin-left:8px}
.cl-text{font:400 15px/1.5 var(--serif);color:var(--ink-2);margin-top:4px;max-width:70ch;
 overflow-wrap:anywhere}
.cl-note{font:400 13px/1.55 var(--sans);color:var(--ink-2);background:var(--paper-3);
 border-left:3px solid var(--rule-3);padding:8px 12px;margin-top:12px;max-width:64ch}
.cl-acts{display:flex;flex-direction:column;align-items:stretch;gap:12px;margin-top:16px}
.cl-main{display:flex;gap:8px;flex-wrap:wrap}
.cl-quiet{display:flex;gap:8px;align-items:center;padding-top:12px;border-top:1px solid var(--rule)}
.cl-quiet input[type=text]{flex:1;max-width:32ch}
.cl-acts form{display:flex;align-items:center;gap:8px}
.cl-acts button{font:600 13px/1 var(--sans);padding:8px 16px;border:1px solid var(--rule-3);background:var(--paper-2);color:var(--ink-2);cursor:pointer;transition:background 160ms cubic-bezier(0.16,1,0.3,1),color 160ms cubic-bezier(0.16,1,0.3,1)}
.cl-acts button:hover{background:var(--paper-3);color:var(--ink)}
.cl-acts button.go{color:var(--paper);background:var(--ink);border-color:var(--ink)}
.cl-acts button.go:hover{background:var(--ink-2)}
.cl-acts input[type=text]{width:210px;padding:8px 8px;border:1px solid var(--rule-3);
 background:var(--paper);color:inherit;font:400 13px/1.4 var(--sans)}
.cl-keeps{display:flex;flex-direction:column;gap:8px;margin-top:16px;max-width:70ch}
.cl-keeps form{display:flex;align-items:baseline;gap:12px}
.cl-keeps button{font:600 13px/1 var(--sans);padding:8px 16px;border:1px solid var(--ink);background:var(--ink);color:var(--paper);cursor:pointer;transition:background 160ms cubic-bezier(0.16,1,0.3,1)}
.cl-keeps button:hover{background:var(--ink-2)}
.cl-keep-text{font:400 13px/1.5 var(--serif);color:var(--ink-2);overflow-wrap:anywhere}
.cl-state{font:400 13px/1.5 var(--mono);color:var(--ink-3);margin-top:8px}
.cl-done{font:400 13px/1.55 var(--sans);color:var(--ink-2);background:var(--paper-3);
 border-left:3px solid var(--rule-3);padding:8px 12px;margin-top:12px;max-width:64ch}
@media (max-width:860px){
 .cl-page{padding:12px 12px 48px}
 .cl-acts input[type=text]{width:100%}
 .cl-keeps form{flex-direction:column;align-items:flex-start;gap:4px}
}";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authserver::session::Sessions;
    use crate::domain::cleanup::Disposition;
    use chrono::Utc;

    fn health() -> Health {
        Health {
            key_verified: true,
            keys_configured: true,
            embedder: "bge-small-en-v1.5".into(),
            degraded_embedder: false,
            last_write: None,
            now: Utc::now(),
        }
    }

    fn token(action: &str, id: &str) -> String {
        format!("tok-{action}-{id}")
    }

    fn member(id: &str, disposition: Disposition, seen: &str) -> Member {
        Member {
            memory_id: id.into(),
            disposition,
            seen_content: seen.into(),
            current_content: Some(seen.into()),
            superseded_by: None,
        }
    }

    fn proposal(kind: CleanupKind, state: &str) -> Proposal {
        Proposal {
            id: "11111111-1111-4111-8111-111111111111".into(),
            kind,
            namespace: "user:me".into(),
            keep_id: match kind.has_keep() {
                true => Some("aaaaaaaa-1111-4111-8111-111111111111".into()),
                false => None,
            },
            rationale: "both rows say the port is 8787".into(),
            produced_by: "qwen/qwen3.7-flash".into(),
            similarity: Some(0.942),
            state: state.into(),
            reason: None,
            decided_at: None,
            created_at: Utc::now(),
            members: vec![
                member("aaaaaaaa-1111-4111-8111-111111111111", Disposition::Keep, "the port is 8787"),
                member("bbbbbbbb-2222-4222-8222-222222222222", Disposition::Retire, "port 8787"),
            ],
        }
    }

    /// The trap this page exists to avoid. Console handlers do not go through `ingest_ctx`, so the
    /// principal built here is the whole of the authorization for an apply, and a `stale` proposal
    /// deletes rows.
    #[test]
    fn the_operator_carries_delete_for_the_one_kind_that_deletes() {
        let reader = Principal {
            client: "lumberroom-console".into(),
            token_id: "console".into(),
            mode: "console",
            scopes: vec![],
            read: vec![NamespaceGrant::new("*", Sensitivity::Sealed)],
            write: vec![],
            registry_write: false,
            sealed_capable: false,
            may_delete: false,
            may_ingest: false,
            may_read_history: true,
        };

        for kind in [CleanupKind::Exact, CleanupKind::Paraphrase, CleanupKind::Contradiction] {
            let p = operator(reader.clone(), kind);
            assert!(!p.may_delete, "{kind} supersedes into a survivor and removes nothing");
            assert!(!p.write.is_empty(), "{kind} still has to be able to supersede");
        }

        let stale = operator(reader.clone(), CleanupKind::Stale);
        assert!(stale.may_delete, "a stale proposal deletes, and forget::by_id refuses without it");

        // Nothing else widens. The registry and the ingest gate are not this surface's business.
        for kind in [CleanupKind::Exact, CleanupKind::Stale] {
            let p = operator(reader.clone(), kind);
            assert!(!p.registry_write);
            assert!(!p.may_ingest);
            assert!(!p.sealed_capable);
        }
    }

    /// A contradiction refuses to apply inside the service, so the page must not offer the button.
    #[test]
    fn a_contradiction_gets_no_apply_button_and_the_page_says_why() {
        let html =
            listing_html(&[proposal(CleanupKind::Contradiction, "proposed")], &health(), &token, None);
        assert!(!html.contains("/apply\""), "a contradiction cannot be applied: {html}");
        assert!(html.contains("/reject\""), "it can still be refused");
        assert!(html.contains("A contradiction names no survivor"));
        assert!(html.contains("yours to call"));
    }

    #[test]
    fn every_other_kind_gets_one() {
        for kind in [CleanupKind::Exact, CleanupKind::Paraphrase, CleanupKind::Stale] {
            let html = listing_html(&[proposal(kind, "proposed")], &health(), &token, None);
            assert!(html.contains("/apply\""), "{kind} has something to apply");
        }
        // A delete reads differently from a retirement, and the button says which it is.
        let stale = listing_html(&[proposal(CleanupKind::Stale, "proposed")], &health(), &token, None);
        assert!(stale.contains("Apply, deleting"));
    }

    /// Every class the page renders has a rule behind it.
    ///
    /// The bug this exists for shipped: the contradiction controls were written with `cl-keeps` and
    /// `cl-keep-text` and neither had a rule, so the buttons rendered as raw browser controls jammed
    /// against unwrapped text. Nothing failed. The markup was right, the handler was right, the
    /// tests asserted on the HTML and passed, and the page was unusable.
    ///
    /// Rendered rather than grepped out of the source, so a class only reachable at runtime, in one
    /// kind or one state, is covered too.
    #[test]
    fn every_class_the_page_renders_has_a_rule_behind_it() {
        let mut html = String::new();
        for kind in [
            CleanupKind::Exact,
            CleanupKind::Paraphrase,
            CleanupKind::Contradiction,
            CleanupKind::Stale,
        ] {
            for state in ["proposed", "applied", "rejected", "obsolete"] {
                html.push_str(&listing_html(&[proposal(kind, state)], &health(), &token, None));
            }
        }
        html.push_str(&listing_html(&[], &health(), &token, Some("applied")));

        let mut undefined: Vec<String> = Vec::new();
        let mut rest = html.as_str();
        while let Some(at) = rest.find("class=\"") {
            rest = &rest[at + 7..];
            let Some(end) = rest.find('"') else { break };
            for class in rest[..end].split_whitespace() {
                let rule = format!(".{class}");
                // A rule can be `.x{`, `.x `, `.x,` or `.x:hover`, so match the name and then check
                // the next character cannot continue an identifier.
                let defined = STYLE.match_indices(&rule).any(|(i, _)| {
                    STYLE[i + rule.len()..]
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_alphanumeric() && c != '-' && c != '_')
                });
                if !defined && !undefined.iter().any(|u| u == class) {
                    undefined.push(class.to_string());
                }
            }
            rest = &rest[end..];
        }
        assert!(
            undefined.is_empty(),
            "these classes are rendered and have no rule, so whatever they were meant to lay out \
             does not: {undefined:?}"
        );
    }

    #[test]
    fn a_decided_finding_carries_no_controls_at_all() {
        for state in ["applied", "rejected", "obsolete"] {
            let html =
                listing_html(&[proposal(CleanupKind::Paraphrase, state)], &health(), &token, None);
            assert!(!html.contains("<form"), "{state} is decided and offers no button: {html}");
            assert!(html.contains(state), "and the page says which it is");
        }
    }

    /// `close_answered` writes `obsolete` when the owner resolves a contradiction by hand. A page
    /// that knew three states would drop those rows without saying so.
    #[test]
    fn a_closed_finding_is_shown_rather_than_dropped() {
        let html = listing_html(&[proposal(CleanupKind::Contradiction, "obsolete")], &health(), &token, None);
        assert!(html.contains("Closed"));
        assert!(html.contains("the store answered these"));
        assert!(html.contains("both rows say the port is 8787"));
    }

    #[test]
    fn each_member_is_named_with_its_disposition_and_links_to_the_row() {
        let html = listing_html(&[proposal(CleanupKind::Paraphrase, "proposed")], &health(), &token, None);
        assert!(html.contains("href=\"/console/fact/aaaaaaaa-1111-4111-8111-111111111111\""));
        assert!(html.contains("href=\"/console/fact/bbbbbbbb-2222-4222-8222-222222222222\""));
        assert!(html.contains(">keep<"));
        assert!(html.contains(">retire<"));
        assert!(html.contains("the port is 8787"));
        assert!(html.contains("0.942"), "the score that grouped the cluster");
        assert!(html.contains("qwen/qwen3.7-flash"), "and what produced it");
        assert!(html.contains("user:me"));
    }

    /// The three marks, in the order `lumberroom cleanup show` prints them. A row that is gone is
    /// gone whatever else was true of it.
    #[test]
    fn a_member_the_store_has_moved_is_marked_the_way_the_command_line_marks_it() {
        let mut gone = member("a", Disposition::Retire, "the port is 8787");
        gone.current_content = None;
        gone.superseded_by = Some("c".into());
        assert_eq!(member_mark(&gone), Some("GONE"));

        let mut retired = member("a", Disposition::Retire, "the port is 8787");
        retired.superseded_by = Some("c".into());
        retired.current_content = Some("something else".into());
        assert_eq!(member_mark(&retired), Some("ALREADY RETIRED"));

        let mut edited = member("a", Disposition::Retire, "the port is 8787");
        edited.current_content = Some("the port is 8080".into());
        assert_eq!(member_mark(&edited), Some("EDITED SINCE"));

        assert_eq!(member_mark(&member("a", Disposition::Retire, "unchanged")), None);
    }

    #[test]
    fn a_mark_reaches_the_page() {
        let mut p = proposal(CleanupKind::Paraphrase, "proposed");
        p.members[1].current_content = None;
        let html = listing_html(&[p], &health(), &token, None);
        assert!(html.contains("GONE"), "the page says in advance what the button would say after");
    }

    /// The rationale is a model's sentence about the owner's own rows, so it is untrusted text.
    /// Both directions: the escaped value whole, and the raw payload nowhere.
    #[test]
    fn a_hostile_rationale_renders_as_inert_text() {
        let payload = "\"<script>alert(1)</script>";
        let mut p = proposal(CleanupKind::Paraphrase, "proposed");
        p.rationale = payload.into();
        p.produced_by = payload.into();
        p.members[0].seen_content = payload.into();
        let html = listing_html(&[p], &health(), &token, None);

        assert!(
            html.contains("&quot;&lt;script&gt;alert(1)&lt;/script&gt;"),
            "the value has to survive whole as text: {html}"
        );
        assert!(!html.contains(payload), "and the raw payload appears nowhere");
        assert!(!html.contains("<script>alert"), "no script tag as markup");
    }

    #[test]
    fn the_page_fetches_nothing_and_runs_nothing() {
        for html in [
            listing_html(&[proposal(CleanupKind::Paraphrase, "proposed")], &health(), &token, Some("applied")),
            listing_html(&[], &health(), &token, None),
        ] {
            assert!(!html.contains("<script"), "no JavaScript");
            assert_eq!(
                html.matches("src=").count(),
                html.matches("src=\"/console/logo.svg\"").count(),
                "nothing is fetched except the mark"
            );
            assert!(!html.contains("http://"), "no external asset");
            assert!(html.starts_with("<!doctype html>"));
        }
    }

    #[test]
    fn an_empty_queue_says_what_a_pass_is_for() {
        let html = listing_html(&[], &health(), &token, None);
        assert!(html.contains("Nothing has piled up."));
        assert!(html.contains("lumberroom cleanup run"));
        assert!(html.contains("nothing found yet"));
        assert!(!html.contains("Waiting"), "no empty heading over nothing");
    }

    #[test]
    fn the_count_leads_with_what_is_still_waiting() {
        let rows =
            [proposal(CleanupKind::Paraphrase, "proposed"), proposal(CleanupKind::Exact, "applied")];
        let html = listing_html(&rows, &health(), &token, None);
        assert!(html.contains("1 finding waiting of 2"), "{html}");
        assert_eq!(counted(0, 4), "nothing waiting, 4 decided");
        assert_eq!(counted(3, 9), "3 findings waiting of 9");
    }

    #[test]
    fn the_done_line_prints_only_words_this_file_redirected_with() {
        assert!(done_note(Some("applied")).contains("Applied."));
        assert!(done_note(Some("rejected")).contains("Rejected."));
        assert_eq!(done_note(Some("<script>")), "");
        assert_eq!(done_note(None), "");
    }

    #[test]
    fn a_cluster_a_model_grouped_says_so_rather_than_printing_a_zero() {
        assert_eq!(similarity(Some(0.942)), "0.942");
        assert_eq!(similarity(Some(0.9)), "0.900");
        assert_eq!(similarity(None), "no score");
        let mut p = proposal(CleanupKind::Contradiction, "proposed");
        p.similarity = None;
        assert!(listing_html(&[p], &health(), &token, None).contains("no score"));
    }

    /// Every control posts a token bound to the session, the act and the finding. All four
    /// directions, because this page draws two acts over many rows at once and a token that
    /// crossed either boundary would retire rows the owner never looked at.
    #[test]
    fn a_token_decides_one_finding_and_one_act_and_nothing_else() {
        let sessions = Sessions::new("k".repeat(32), 900, true);
        let session = sessions.open(&sessions.issue(1_000), 1_001).expect("a live session");
        let (mine, theirs) = ("row-a", "row-b");

        assert!(!sessions.console_csrf_ok(&session, APPLY_ACTION, mine, ""));

        let applying = sessions.console_csrf(&session, APPLY_ACTION, mine);
        assert!(sessions.console_csrf_ok(&session, APPLY_ACTION, mine, &applying));
        assert!(
            !sessions.console_csrf_ok(&session, APPLY_ACTION, theirs, &applying),
            "the queue prints every finding at once, so a token has to name its own"
        );
        assert!(
            !sessions.console_csrf_ok(&session, REJECT_ACTION, mine, &applying),
            "a token that carries a finding out must not refuse one"
        );

        let rejecting = sessions.console_csrf(&session, REJECT_ACTION, mine);
        assert!(
            !sessions.console_csrf_ok(&session, APPLY_ACTION, mine, &rejecting),
            "and a token that refuses one must not retire rows"
        );

        // A token minted to hand an OAuth client the whole store is signed under another label.
        let consent = sessions.csrf(&session, APPLY_ACTION, mine, "chal", "st");
        assert!(!sessions.console_csrf_ok(&session, APPLY_ACTION, mine, &consent));

        let other = sessions.open(&sessions.issue(2_000), 2_001).expect("a second session");
        assert!(!sessions.console_csrf_ok(&other, APPLY_ACTION, mine, &applying));
    }

    #[test]
    fn the_two_acts_never_share_an_action_string() {
        assert_ne!(APPLY_ACTION, REJECT_ACTION);
        // And neither collides with the ingest queue's, which mints against the same label and the
        // same session over uuids drawn from a different table.
        for other in ["approve", "reject", "unreject", "write", "alias-record", "alias-forget"] {
            assert_ne!(APPLY_ACTION, other);
            assert_ne!(REJECT_ACTION, other);
        }
    }
}
