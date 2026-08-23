//! The five tools behind a capability, on the same surface as the four open ones.
//!
//! They live in their own file rather than in `mod.rs` because `mod.rs` is the composition point
//! several tracks edit at once. `#[tool_router(router = extra_tool_router)]` builds a second router
//! that `Lumberroom::new` adds to the first, so registration is one `+` rather than five pasted methods.
//!
//! Nothing here decides anything. Each handler parses its arguments and calls the service that
//! already holds the grant check, which is what keeps the refusal identical whether a call arrives
//! through MCP, through `/admin`, or through the console. The capability table in
//! `super::capability` keeps a tool out of the list a client reads; `history::of`,
//! `registry::history`, `registry::set` and `alias::put` are what refuse a client that names the
//! tool anyway.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, ErrorData as McpError, RoleServer};
use schemars::JsonSchema;
use serde::Deserialize;

use super::tools;
use super::Lumberroom;
use crate::domain::errors::DomainError;
use crate::domain::namespaces;
use crate::services::{alias, history, registry};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryHistoryArgs {
    /// id of the fact whose versions you want, as returned by memory_search or memory_write.
    pub id: String,
    /// Validated when present and it narrows nothing: a correction may move a fact to another
    /// namespace, so the walk crosses namespaces by design.
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RegistryHistoryArgs {
    /// 'host', 'service', 'credential-ref', 'model-route' or 'dataset'.
    pub kind: String,
    /// Exact key, the same one registry_get takes.
    pub key: String,
    /// Where to look. Omit to check the project, then the user namespace, then global.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Versions to return. Default 20, capped at 200.
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RegistrySetArgs {
    /// 'host', 'service', 'credential-ref', 'model-route' or 'dataset'.
    pub kind: String,
    /// The canonical key, dotted and lowercase: 'services.lumberroom.port', not 'lumberroom port'.
    pub key: String,
    /// The value, as JSON. A string, a number, or an object when the fact has parts.
    pub value: serde_json::Value,
    /// Where it belongs: 'global' for infrastructure, 'project:<slug>' for one codebase,
    /// 'user:me' for the person.
    pub namespace: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AliasSetArgs {
    /// The other name for the subject, as somebody would type it.
    pub alias: String,
    /// The name the group is keyed on, which is what the subject is called now.
    pub canonical: String,
    /// The namespace holding facts about this subject, usually 'project:<slug>'.
    pub namespace: String,
    /// When the alias started denoting the subject. A date, `2026-03-01`, or a full RFC 3339
    /// instant. Set it only when the user stated the time; omit it otherwise.
    #[serde(default)]
    pub since: Option<String>,
    /// When it stopped. Same two forms as since, and the same rule: only what the user stated.
    #[serde(default)]
    pub until: Option<String>,
    /// 'manual' when the user stated the two names are the same thing, 'derived' when something
    /// read the pair out of a fact. Defaults to manual.
    #[serde(default)]
    pub origin: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AliasListArgs {
    /// One namespace to list. Omit for every namespace you may read.
    #[serde(default)]
    pub namespace: Option<String>,
}

#[tool_router(router = extra_tool_router, vis = "pub(crate)")]
impl Lumberroom {
    #[tool(
        name = "memory_history",
        description = "Every version of one fact, oldest first, the versions a later correction \
retired included. Call it when the user asks what was believed before, or when a fact looks wrong \
and you need to see what it replaced. It takes a memory id from memory_search or memory_write and \
never a phrase. Versions your credential may not read are counted in withheld rather than shown, \
so a chain with a withheld count is a partial answer: say so rather than reading it as the whole \
story. One indexed walk, bounded by a depth cap it reports as depth_capped."
    )]
    async fn memory_history(
        &self,
        Parameters(args): Parameters<MemoryHistoryArgs>,
        rc: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // No namespace recorded against the call: the walk crosses namespaces, so the argument
        // would file the call under one of several the answer came from.
        self.run("memory_history", None, &rc, |ctx| async move {
            let id = uuid::Uuid::parse_str(args.id.trim()).map_err(|_| {
                DomainError::validation(format!("{:?} is not a memory id", args.id))
            })?;
            if let Some(ns) = args.namespace.as_deref() {
                namespaces::normalize(ns)?;
            }
            let timeline = history::of(&ctx, id).await?;
            let json = serde_json::to_value(&timeline).unwrap_or_default();
            Ok((serde_json::to_string_pretty(&json).unwrap_or_default(), json))
        })
        .await
    }

    #[tool(
        name = "registry_history",
        description = "What a registry key used to hold, newest first. The value it holds now is \
not in the answer: registry_get is one call away for that, and this is what the key stopped \
holding. Call it when an operational value changed and the old one still matters, as in \"where did \
the backups live before we moved them\". A key reached through a redirect answers here too, and \
resolved_from names the key the versions came from. Bounded to 20 versions unless you ask for \
more, 200 at most."
    )]
    async fn registry_history(
        &self,
        Parameters(args): Parameters<RegistryHistoryArgs>,
        rc: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let namespace = args.namespace.clone();
        self.run("registry_history", namespace, &rc, |ctx| async move {
            let result = registry::history(
                &ctx,
                &args.kind,
                &args.key,
                args.namespace.as_deref(),
                // No project argument on this surface yet. registry_get takes one and this does
                // not, so a caller that wants the project's own history names the namespace.
                None,
                args.limit,
            )
            .await?;
            let json = serde_json::to_value(&result).unwrap_or_default();
            Ok((serde_json::to_string_pretty(&json).unwrap_or_default(), json))
        })
        .await
    }

    #[tool(
        name = "registry_set",
        description = "Record an exact operational value under a canonical key: a host, a service \
endpoint, where a credential lives, a model route, a dataset. Use it when the user states \
something another tool will act on and a wrong guess would break; use memory_write for anything a \
person would say in a sentence. Keys are canonical and dotted, 'services.lumberroom.port' rather than \
'lumberroom port', and a key that gets rejected is remembered as a redirect so the next caller reaching \
for the same wrong name lands on the right row instead of inventing a third. Never put a secret in \
the value. Record a credential-ref naming where the secret lives, and leave the secret where it is."
    )]
    async fn registry_set(
        &self,
        Parameters(args): Parameters<RegistrySetArgs>,
        rc: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let namespace = Some(args.namespace.clone());
        self.run("registry_set", namespace, &rc, |ctx| async move {
            let result = registry::set(
                &ctx,
                &args.namespace,
                &args.kind,
                args.key.trim(),
                &args.value,
                // The namespace default decides the level. A tool argument that could raise it
                // belongs with the operator surfaces until somebody needs it here.
                None,
                None,
            )
            .await?;
            let json = serde_json::to_value(&result).unwrap_or_default();
            Ok((serde_json::to_string_pretty(&json).unwrap_or_default(), json))
        })
        .await
    }

    #[tool(
        name = "alias_set",
        description = "Record that two names mean the same subject, so a search for either one \
finds the facts written under the other. Renames are the case: a project called Warden, then \
Quill, then Lumen, with facts filed under all three and a search for the current name finding a \
third of them. canonical is what the subject is called now and alias is the other name. This \
steers every later search for every client on this server, so record it when the user has said the \
two names are the same thing and never from a resemblance you noticed."
    )]
    async fn alias_set(
        &self,
        Parameters(args): Parameters<AliasSetArgs>,
        rc: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let namespace = Some(args.namespace.clone());
        self.run("alias_set", namespace, &rc, |ctx| async move {
            let since = instant("since", args.since.as_deref())?;
            let until = instant("until", args.until.as_deref())?;
            let record = alias::put(
                &ctx,
                ctx.repos.aliases.as_ref(),
                &args.namespace,
                &args.alias,
                &args.canonical,
                since,
                until,
                args.origin.as_deref(),
            )
            .await?;
            let json = serde_json::to_value(&record).unwrap_or_default();
            Ok((serde_json::to_string_pretty(&json).unwrap_or_default(), json))
        })
        .await
    }

    #[tool(
        name = "alias_list",
        description = "Every pair of names recorded as meaning the same subject. Call it before \
recording a new alias, and when a search comes back thinner than the store should hold and you \
suspect the subject is filed under a name nobody mentioned. Namespaces your credential cannot read \
are absent, so this is what you may see rather than everything there is."
    )]
    async fn alias_list(
        &self,
        Parameters(args): Parameters<AliasListArgs>,
        rc: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let namespace = args.namespace.clone();
        self.run("alias_list", namespace, &rc, |ctx| async move {
            // `alias::list` drops every namespace this caller may not read, and that filter is the
            // whole of the tool. A name is a disclosure a content filter never sees: an unfiltered
            // list hands a narrow credential the names of namespaces it cannot open.
            let rows =
                alias::list(&ctx, ctx.repos.aliases.as_ref(), args.namespace.as_deref()).await?;
            let json = serde_json::json!({ "aliases": rows });
            Ok((serde_json::to_string_pretty(&json).unwrap_or_default(), json))
        })
        .await
    }
}

/// One instant argument, named in its own refusal.
///
/// `tools::parse_occurred_at` is the parser, and its message names `occurred_at` because that is
/// the argument it was written for. A model told to fix `occurred_at` on a call that has no such
/// argument fixes nothing, so the field it actually sent goes in the message here.
fn instant(
    field: &str,
    raw: Option<&str>,
) -> crate::domain::errors::Result<Option<chrono::DateTime<chrono::Utc>>> {
    let Some(value) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    tools::parse_occurred_at(value).map(Some).map_err(|_| {
        DomainError::validation(format!(
            "{field} `{}` is not one of the two accepted forms. Pass a date, `2026-03-01`, read as \
midnight UTC, or a full RFC 3339 instant, `2026-03-01T09:30:00Z`. Omit it rather than choosing a \
day the user did not state.",
            value.chars().take(60).collect::<String>()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::capability::TOOL_CAPABILITIES;

    /// The table and the router, held against each other.
    ///
    /// `capability::required` answers `Open` for a tool it has never heard of, which publishes an
    /// ungated tool to every client. This is the guard that turns that into a failing build. It
    /// fails in the other direction too: an entry for a tool nobody registered means the
    /// documentation generated from this table describes a tool that does not exist.
    #[test]
    fn the_capability_table_names_every_registered_tool_and_nothing_else() {
        let router = Lumberroom::tool_router() + Lumberroom::extra_tool_router();
        let mut registered: Vec<String> =
            router.list_all().into_iter().map(|t| t.name.to_string()).collect();
        registered.sort();
        let mut declared: Vec<String> =
            TOOL_CAPABILITIES.iter().map(|(name, _)| (*name).to_string()).collect();
        declared.sort();
        assert_eq!(
            registered, declared,
            "a tool missing from the table ships ungated, and an entry with no tool documents one \
             that does not exist"
        );
    }

    #[test]
    fn an_instant_argument_is_refused_by_the_name_the_caller_sent() {
        assert!(instant("since", None).unwrap().is_none());
        assert!(instant("since", Some("  ")).unwrap().is_none());
        assert!(instant("until", Some("2026-03-01")).unwrap().is_some());
        assert!(instant("since", Some("2026-03-01T09:30:00Z")).unwrap().is_some());

        let refused = instant("since", Some("last March")).unwrap_err();
        let message = refused.client_message();
        assert!(message.contains("since"), "{message}");
        assert!(!message.contains("occurred_at"), "{message}");
    }
}
