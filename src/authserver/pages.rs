//! The two pages the owner ever sees: sign in, and decide what a client may reach.
//!
//! Server-rendered, no JavaScript, no external assets, inline CSS. A consent screen that depends on
//! a CDN is a consent screen that renders blank on a network that blocks the CDN, and the owner reads
//! this page while deciding whether to hand a browser tool their memory.
//!
//! Every interpolated value goes through `escape`. `client_name`, `software_id` and the redirect URI
//! arrive in an unauthenticated dynamic-registration request, so they are attacker-controlled text
//! rendered on a page that has the owner's session cookie. That is the one XSS this server can
//! actually be handed.

use crate::console::pages::FAVICON;
use crate::domain::oauth::GrantProfile;

/// The mark, above the heading on all three pages.
///
/// It points at `/console/logo.svg`, which the console serves outside its session guard for exactly
/// this: the owner reads these two screens before any session exists. One path rather than a second
/// copy of the file, because a mark that drifts between the consent screen and the console is a
/// mark the owner has to think about.
const MARK: &str = "<img class=\"glyph\" src=\"/console/logo.svg\" width=\"32\" height=\"32\" alt=\"\">";

/// The hidden fields that carry an authorization request through the login and consent POSTs.
///
/// They are hidden fields rather than a server-side pending-request table because the request has to
/// survive a restart between the redirect and the Allow, and because every value in it is
/// re-validated against the client record on the way back in. Nothing here is trusted for having
/// been on the page.
pub struct FlowFields<'a> {
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub code_challenge: &'a str,
    pub code_challenge_method: &'a str,
    pub response_type: &'a str,
    pub state: Option<&'a str>,
    pub scope: Option<&'a str>,
    pub resource: Option<&'a str>,
}

/// What the owner is shown about the client asking for access.
pub struct ClientView<'a> {
    pub client_name: &'a str,
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub software_id: Option<&'a str>,
    /// True when the client registered itself through RFC 7591 rather than being issued by hand.
    pub self_registered: bool,
    /// The profile this client already holds, when it is being re-consented.
    pub current_profile: Option<&'a str>,
}

const STYLE: &str = "\
:root{color-scheme:light dark}
*{box-sizing:border-box}
body{margin:0;padding:2.5rem 1.25rem;background:#f6f6f4;color:#17181a;
 font:16px/1.55 ui-sans-serif,-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif}
main{max-width:34rem;margin:0 auto;background:#fff;border:1px solid #dcdcd6;border-radius:10px;
 padding:1.75rem}
h1{margin:0 0 .35rem;font-size:1.3rem;letter-spacing:-.01em}
p{margin:.5rem 0}
.lede{color:#4a4d52;margin-bottom:1.25rem}
.mark{font-size:.72rem;letter-spacing:.14em;text-transform:uppercase;color:#83868c;margin:0 0 1rem}
.glyph{display:block;width:32px;height:32px;margin:0 0 .6rem}
dl{margin:0 0 1.25rem;padding:.85rem 1rem;background:#f6f6f4;border-radius:8px;font-size:.9rem}
dt{color:#6a6d73;font-size:.78rem;text-transform:uppercase;letter-spacing:.06em}
dd{margin:.1rem 0 .7rem;word-break:break-all;font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
dd:last-of-type{margin-bottom:0}
fieldset{border:1px solid #dcdcd6;border-radius:8px;padding:.85rem 1rem;margin:0 0 1.25rem}
legend{padding:0 .35rem;font-size:.8rem;text-transform:uppercase;letter-spacing:.06em;color:#6a6d73}
.choice{display:block;padding:.6rem 0;border-bottom:1px solid #ececE6}
.choice:last-child{border-bottom:0}
.choice input{margin-right:.5rem}
.choice b{font-weight:600}
.choice span{display:block;margin:.15rem 0 0 1.4rem;color:#4a4d52;font-size:.88rem}
label.field{display:block;margin:0 0 .35rem;font-weight:600;font-size:.9rem}
input[type=password]{width:100%;padding:.6rem .7rem;font-size:1rem;border:1px solid #b9bbc0;
 border-radius:7px;background:#fff;color:inherit}
.actions{display:flex;gap:.6rem;flex-wrap:wrap}
button{font:inherit;font-weight:600;padding:.62rem 1.15rem;border-radius:7px;border:1px solid #17181a;
 background:#17181a;color:#fff;cursor:pointer}
button.secondary{background:#fff;color:#17181a;border-color:#b9bbc0}
.warn{padding:.7rem .9rem;border-radius:8px;background:#fdf1e7;border:1px solid #eccfae;
 font-size:.9rem;margin:0 0 1.1rem}
.error{padding:.7rem .9rem;border-radius:8px;background:#fdeaea;border:1px solid #e9b7b7;
 font-size:.9rem;margin:0 0 1.1rem}
.foot{margin:1.25rem 0 0;color:#83868c;font-size:.82rem}
@media (prefers-color-scheme:dark){
 body{background:#111214;color:#eceded}
 main{background:#191b1e;border-color:#2c2f34}
 dl,fieldset{background:#15171a;border-color:#2c2f34}
 .lede,.choice span{color:#a9acb2}
 input[type=password]{background:#111214;border-color:#3a3e44;color:inherit}
 button{background:#eceded;color:#111214;border-color:#eceded}
 button.secondary{background:transparent;color:#eceded;border-color:#3a3e44}
 .warn{background:#2a2013;border-color:#5c4520}
 .error{background:#2a1414;border-color:#5c2020}
}";

/// HTML-escape for both text and attribute contexts.
///
/// Quotes and apostrophes are escaped too, because the same function fills `value="..."` on every
/// hidden field. Escaping only `<` and `&` is what makes an attacker-supplied client name able to
/// close an attribute and open an event handler.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn shell(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<meta name=\"robots\" content=\"noindex,nofollow\">{FAVICON}\
<title>{}</title><style>{STYLE}</style></head>\n<body><main>{body}</main></body></html>\n",
        escape(title)
    )
}

fn hidden(name: &str, value: &str) -> String {
    format!(
        "<input type=\"hidden\" name=\"{}\" value=\"{}\">",
        escape(name),
        escape(value)
    )
}

impl FlowFields<'_> {
    /// The authorization request, re-posted verbatim. An absent optional field is omitted rather
    /// than sent empty, so what comes back deserialises to the same `None` it arrived as.
    fn hidden_inputs(&self) -> String {
        let mut out = String::new();
        out.push_str(&hidden("client_id", self.client_id));
        out.push_str(&hidden("redirect_uri", self.redirect_uri));
        out.push_str(&hidden("code_challenge", self.code_challenge));
        out.push_str(&hidden("code_challenge_method", self.code_challenge_method));
        out.push_str(&hidden("response_type", self.response_type));
        for (name, value) in
            [("state", self.state), ("scope", self.scope), ("resource", self.resource)]
        {
            if let Some(v) = value {
                out.push_str(&hidden(name, v));
            }
        }
        out
    }
}

/// The password form. Shown when there is no live session, and again with a message when a password
/// was wrong or the attempt was throttled.
pub fn login(flow: &FlowFields, client_name: &str, error: Option<&str>) -> String {
    let banner = match error {
        Some(message) => format!("<p class=\"error\">{}</p>", escape(message)),
        None => String::new(),
    };
    shell(
        "lumberroom: sign in",
        &format!(
            "{MARK}<p class=\"mark\">lumberroom</p>\
<h1>Sign in to lumberroom</h1>\
<p class=\"lede\"><b>{client}</b> is asking for access to your memory. Sign in to decide what it \
may reach.</p>\
{banner}\
<form method=\"post\" action=\"/oauth/login\">{fields}\
<label class=\"field\" for=\"password\">Owner password</label>\
<input id=\"password\" name=\"password\" type=\"password\" autocomplete=\"current-password\" \
autofocus required>\
<p class=\"actions\"><button type=\"submit\">Sign in</button></p></form>\
<p class=\"foot\">Signing in does not grant anything. The next page is where you choose.</p>",
            client = escape(client_name),
            banner = banner,
            fields = flow.hidden_inputs(),
        ),
    )
}

/// The consent screen. The only place in this server that turns a registered client into a client
/// holding a grant.
pub fn consent(
    flow: &FlowFields,
    client: &ClientView,
    csrf: &str,
    default_profile: GrantProfile,
) -> String {
    let mut choices = String::new();
    for profile in [GrantProfile::Full, GrantProfile::Standard, GrantProfile::Narrow] {
        let checked = if profile == default_profile { " checked" } else { "" };
        choices.push_str(&format!(
            "<label class=\"choice\"><input type=\"radio\" name=\"profile\" value=\"{value}\"{checked}>\
<b>{name}</b><span>{describe}</span></label>",
            value = escape(profile.as_str()),
            checked = checked,
            name = escape(&title_case(profile.as_str())),
            describe = escape(profile.describe()),
        ));
    }

    // The origin of the client is the single most useful thing on this page. A self-registered
    // client chose its own name, and the name is the only thing about it the owner recognises.
    let origin = if client.self_registered {
        "<p class=\"warn\">This client registered itself. Its name and icon are whatever it sent, \
and nothing has checked them. Grant it only what you would grant a stranger with that name.</p>"
            .to_string()
    } else {
        String::new()
    };

    let again = match client.current_profile {
        Some(profile) => format!(
            "<p class=\"lede\">This client already holds the <b>{}</b> grant. Choosing again \
replaces it.</p>",
            escape(profile)
        ),
        None => String::new(),
    };

    let software = match client.software_id {
        Some(id) if !id.is_empty() => {
            format!("<dt>Software id</dt><dd>{}</dd>", escape(id))
        }
        _ => "<dt>Software id</dt><dd>not declared</dd>".to_string(),
    };

    shell(
        "lumberroom: grant access",
        &format!(
            "{MARK}<p class=\"mark\">lumberroom</p>\
<h1>Give <b>{client}</b> access to lumberroom?</h1>\
<p class=\"lede\">It will be able to read and write memories inside the boundary you pick, on \
every surface, until you revoke it.</p>\
{origin}{again}\
<dl><dt>Client name</dt><dd>{client}</dd>\
<dt>Sends the code back to</dt><dd>{redirect}</dd>\
{software}\
<dt>Client id</dt><dd>{client_id}</dd>\
<dt>Registered</dt><dd>{registered}</dd></dl>\
<form method=\"post\" action=\"/oauth/consent\">{fields}{csrf}\
<fieldset><legend>What it may reach</legend>{choices}</fieldset>\
<p class=\"actions\"><button type=\"submit\" name=\"action\" value=\"allow\">Allow</button>\
<button type=\"submit\" name=\"action\" value=\"deny\" class=\"secondary\">Deny</button></p></form>\
<p class=\"foot\">You can change or revoke this later with <code>lumberroom clients</code>.</p>",
            client = escape(client.client_name),
            origin = origin,
            again = again,
            redirect = escape(client.redirect_uri),
            software = software,
            client_id = escape(client.client_id),
            registered = if client.self_registered {
                "by itself, through dynamic client registration"
            } else {
                "by you"
            },
            fields = flow.hidden_inputs(),
            csrf = hidden("csrf", csrf),
            choices = choices,
        ),
    )
}

/// Any failure that must not be reported by redirecting. Everything before the redirect URI has been
/// matched against the client record lands here, because reporting those by redirect is the
/// open-redirect path.
pub fn error_page(title: &str, detail: &str) -> String {
    shell(
        &format!("lumberroom: {title}"),
        &format!(
            "{MARK}<p class=\"mark\">lumberroom</p><h1>{}</h1><p class=\"lede\">{}</p>\
<p class=\"foot\">Nothing was granted, and no code was issued. Start the connection again from \
the client.</p>",
            escape(title),
            escape(detail)
        ),
    )
}

fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow() -> FlowFields<'static> {
        FlowFields {
            client_id: "abc123",
            redirect_uri: "https://claude.ai/api/mcp/auth_callback",
            code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            code_challenge_method: "S256",
            response_type: "code",
            state: Some("st-1"),
            scope: None,
            resource: Some("https://lumberroom.example/mcp"),
        }
    }

    fn view() -> ClientView<'static> {
        ClientView {
            client_name: "Claude",
            client_id: "abc123",
            redirect_uri: "https://claude.ai/api/mcp/auth_callback",
            software_id: Some("anthropic-claude"),
            self_registered: true,
            current_profile: None,
        }
    }

    #[test]
    fn a_client_name_carrying_markup_cannot_close_a_tag() {
        let hostile = "<script>alert(1)</script>";
        let page = login(&flow(), hostile, None);
        assert!(!page.contains("<script>alert"));
        assert!(page.contains("&lt;script&gt;"));
    }

    #[test]
    fn a_client_name_carrying_a_quote_cannot_escape_an_attribute() {
        let hostile = "\" onfocus=\"alert(1)";
        let mut f = flow();
        f.client_id = hostile;
        let page = login(&f, "x", None);
        assert!(!page.contains("onfocus=\"alert"));
        assert!(page.contains("&quot; onfocus=&quot;"));
    }

    #[test]
    fn every_page_is_self_contained() {
        let pages = [
            login(&flow(), "Claude", None),
            consent(&flow(), &view(), "tok", GrantProfile::Standard),
            error_page("bad request", "no"),
        ];
        for page in pages {
            assert!(!page.contains("<script"), "no JavaScript");
            assert!(!page.contains("http://"), "no external assets");
            assert_eq!(
                page.matches("src=").count(),
                page.matches("src=\"/console/logo.svg\"").count(),
                "nothing is fetched except the mark"
            );
            assert!(page.starts_with("<!doctype html>"));
        }
    }

    #[test]
    fn the_login_form_carries_the_authorize_request_forward() {
        let page = login(&flow(), "Claude", None);
        assert!(page.contains("name=\"client_id\" value=\"abc123\""));
        assert!(page.contains("name=\"code_challenge_method\" value=\"S256\""));
        assert!(page.contains("name=\"state\" value=\"st-1\""));
        assert!(page.contains("name=\"resource\" value=\"https://lumberroom.example/mcp\""));
        assert!(page.contains("action=\"/oauth/login\""));
    }

    #[test]
    fn an_absent_optional_parameter_is_omitted_rather_than_sent_empty() {
        // scope is None in the fixture. An empty `scope=` would come back as Some(""), which is a
        // different authorization request from the one the client made.
        let page = login(&flow(), "Claude", None);
        assert!(!page.contains("name=\"scope\""));
    }

    #[test]
    fn the_login_page_shows_a_message_when_one_is_given() {
        let page = login(&flow(), "Claude", Some("that password is wrong"));
        assert!(page.contains("that password is wrong"));
        assert!(login(&flow(), "Claude", None).matches("class=\"error\"").count() == 0);
    }

    #[test]
    fn the_consent_page_offers_all_three_profiles_with_their_descriptions() {
        let page = consent(&flow(), &view(), "tok", GrantProfile::Standard);
        for profile in [GrantProfile::Full, GrantProfile::Standard, GrantProfile::Narrow] {
            assert!(page.contains(&format!("value=\"{}\"", profile.as_str())));
            assert!(page.contains(&escape(profile.describe())));
        }
    }

    #[test]
    fn the_consent_page_preselects_the_configured_default() {
        let page = consent(&flow(), &view(), "tok", GrantProfile::Narrow);
        assert!(page.contains("value=\"narrow\" checked"));
        assert!(!page.contains("value=\"full\" checked"));
    }

    #[test]
    fn the_consent_page_carries_the_csrf_token_and_both_actions() {
        let page = consent(&flow(), &view(), "tok-9", GrantProfile::Full);
        assert!(page.contains("name=\"csrf\" value=\"tok-9\""));
        assert!(page.contains("name=\"action\" value=\"allow\""));
        assert!(page.contains("name=\"action\" value=\"deny\""));
    }

    #[test]
    fn the_consent_page_says_when_a_client_registered_itself() {
        let page = consent(&flow(), &view(), "tok", GrantProfile::Standard);
        assert!(page.contains("registered itself"));
        assert!(page.contains("anthropic-claude"));
        assert!(page.contains("https://claude.ai/api/mcp/auth_callback"));

        let mut issued = view();
        issued.self_registered = false;
        let page = consent(&flow(), &issued, "tok", GrantProfile::Standard);
        assert!(!page.contains("registered itself"));
        assert!(page.contains("by you"));
    }

    #[test]
    fn a_client_being_reconsented_is_told_what_it_already_holds() {
        let mut again = view();
        again.current_profile = Some("narrow");
        let page = consent(&flow(), &again, "tok", GrantProfile::Standard);
        assert!(page.contains("already holds"));
        assert!(page.contains("<b>narrow</b>"));
    }

    #[test]
    fn an_undeclared_software_id_is_shown_as_such_rather_than_as_a_gap() {
        let mut anonymous = view();
        anonymous.software_id = None;
        let page = consent(&flow(), &anonymous, "tok", GrantProfile::Standard);
        assert!(page.contains("not declared"));
    }
}
