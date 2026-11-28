//! A browser for the fake sites: it speaks real HTTP to the loopback servers under made-up
//! public names, keeps one page at a time, numbers the interactive elements like the real
//! driver does and types secrets only where the caller's origin check allows it.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use browser_cdp::{ReadTarget, ReadValue};
use builtin_tools::browser::{BrowserDriver, FieldTarget, Wait, origin_of};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::Value;
use url::Url;

#[derive(Clone, Debug)]
enum El {
    Heading(String),
    Text(String),
    Input {
        name: String,
        kind: String,
        label: String,
        value: String,
        form: usize,
    },
    Button {
        label: String,
        form: Option<usize>,
    },
    Link {
        href: String,
        label: String,
    },
    Code(String),
}

#[derive(Clone, Debug)]
struct Page {
    url: Url,
    status: u16,
    els: Vec<El>,
    actions: Vec<String>,
}

#[derive(Default)]
struct State {
    page: Option<Page>,
    /// What was typed or ticked, by ref.
    typed: HashMap<String, String>,
    history: Vec<Url>,
}

pub struct SiteBrowser {
    http: reqwest::Client,
    ports: HashMap<&'static str, u16>,
    state: Mutex<State>,
    /// `METHOD url` of every navigation, and the ones no site answered.
    pub visited: Mutex<Vec<String>>,
    pub unreachable: Mutex<Vec<String>>,
}

fn re() -> &'static (Regex, Regex) {
    static RE: OnceLock<(Regex, Regex)> = OnceLock::new();
    RE.get_or_init(|| {
        (
            Regex::new(
                r"(?s)<(h1|p|code|button|a)\b([^>]*)>(.*?)</(?:h1|p|code|button|a)>|<input\b([^>]*)>|<form\b([^>]*)>|(</form>)",
            )
            .unwrap(),
            Regex::new(r#"([\w-]+)="([^"]*)""#).unwrap(),
        )
    })
}

fn attrs(s: &str) -> HashMap<String, String> {
    re().1
        .captures_iter(s)
        .map(|c| (c[1].to_owned(), c[2].to_owned()))
        .collect()
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse(url: Url, status: u16, html: &str) -> Page {
    let mut els = Vec::new();
    let mut actions: Vec<String> = Vec::new();
    let mut open_form: Option<usize> = None;
    for c in re().0.captures_iter(html) {
        if let Some(tag) = c.get(1) {
            let a = attrs(&c[2]);
            let inner = strip_tags(&c[3]);
            els.push(match tag.as_str() {
                "h1" => El::Heading(inner),
                "p" => El::Text(inner),
                "code" => El::Code(inner),
                "button" => El::Button {
                    label: inner,
                    form: open_form,
                },
                _ => El::Link {
                    href: a.get("href").cloned().unwrap_or_default(),
                    label: inner,
                },
            });
        } else if let Some(i) = c.get(4) {
            let a = attrs(i.as_str());
            els.push(El::Input {
                name: a.get("name").cloned().unwrap_or_default(),
                kind: a.get("type").cloned().unwrap_or_else(|| "text".into()),
                label: a.get("aria-label").cloned().unwrap_or_default(),
                value: a.get("value").cloned().unwrap_or_default(),
                form: open_form.unwrap_or(usize::MAX),
            });
        } else if let Some(f) = c.get(5) {
            actions.push(attrs(f.as_str()).get("action").cloned().unwrap_or_default());
            open_form = Some(actions.len() - 1);
        } else {
            open_form = None;
        }
    }
    Page {
        url,
        status,
        els,
        actions,
    }
}

/// The element behind each ref, in document order (hidden inputs have none).
fn interactive(p: &Page) -> Vec<(String, &El)> {
    p.els
        .iter()
        .filter(|e| match e {
            El::Input { kind, .. } => kind != "hidden",
            El::Button { .. } | El::Link { .. } | El::Code(_) => true,
            _ => false,
        })
        .enumerate()
        .map(|(i, e)| (format!("e{}", i + 1), e))
        .collect()
}

impl SiteBrowser {
    pub fn new(ports: HashMap<&'static str, u16>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
            ports,
            state: Mutex::default(),
            visited: Mutex::default(),
            unreachable: Mutex::default(),
        }
    }

    async fn load(&self, method: &str, url: &Url, form: Option<String>) -> Result<String, String> {
        let host = url.host_str().unwrap_or_default().to_owned();
        let Some(port) = self.ports.get(host.as_str()) else {
            self.unreachable.lock().push(format!("{method} {url}"));
            return Err(format!("net::ERR_NAME_NOT_RESOLVED ({host})"));
        };
        self.visited.lock().push(format!("{method} {url}"));
        let mut real = format!("http://127.0.0.1:{port}{}", url.path());
        if let Some(q) = url.query() {
            real.push('?');
            real.push_str(q);
        }
        let req = if method == "POST" {
            self.http
                .post(&real)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(form.unwrap_or_default())
        } else {
            self.http.get(&real)
        };
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let html = resp.text().await.map_err(|e| e.to_string())?;
        let page = parse(url.clone(), status, &html);
        let mut st = self.state.lock();
        if let Some(old) = st.page.take() {
            st.history.push(old.url);
        }
        st.typed.clear();
        let out = render(&page, &st.typed);
        st.page = Some(page);
        Ok(out)
    }

    fn resolve(&self, href: &str) -> Result<Url, String> {
        let base = self
            .state
            .lock()
            .page
            .as_ref()
            .map(|p| p.url.clone())
            .ok_or("no page is open")?;
        base.join(href).map_err(|e| e.to_string())
    }

    fn element(&self, r: &str) -> Result<(El, Page), String> {
        let st = self.state.lock();
        let page = st.page.clone().ok_or("no page is open")?;
        interactive(&page)
            .into_iter()
            .find(|(k, _)| k == r)
            .map(|(_, e)| (e.clone(), page.clone()))
            .ok_or_else(|| format!("unknown ref {r}: take a new snapshot"))
    }

    fn origin(&self) -> Option<String> {
        self.state
            .lock()
            .page
            .as_ref()
            .and_then(|p| origin_of(p.url.as_str()))
    }
}

fn render(p: &Page, typed: &HashMap<String, String>) -> String {
    let mut out = format!("url: {}\nstatus: {}\n", p.url, p.status);
    let refs: HashMap<usize, String> = {
        let mut m = HashMap::new();
        let mut n = 0;
        for (i, e) in p.els.iter().enumerate() {
            let shown = match e {
                El::Input { kind, .. } => kind != "hidden",
                El::Button { .. } | El::Link { .. } | El::Code(_) => true,
                _ => false,
            };
            if shown {
                n += 1;
                m.insert(i, format!("e{n}"));
            }
        }
        m
    };
    for (i, e) in p.els.iter().enumerate() {
        match e {
            El::Heading(t) => writeln!(out, "heading \"{t}\"").unwrap(),
            El::Text(t) => writeln!(out, "text \"{t}\"").unwrap(),
            El::Input {
                kind, label, value, ..
            } if kind != "hidden" => {
                let r = &refs[&i];
                let v = typed.get(r).cloned().unwrap_or_else(|| value.clone());
                let role = if kind == "checkbox" {
                    "checkbox"
                } else {
                    "textbox"
                };
                let shown = match (kind.as_str(), v.is_empty()) {
                    (_, true) => String::new(),
                    ("password", false) => " value=\"********\"".to_owned(),
                    (_, false) => format!(" value=\"{v}\""),
                };
                writeln!(out, "[{r}] {role} \"{label}\"{shown}").unwrap();
            }
            El::Button { label, .. } => {
                writeln!(out, "[{}] button \"{label}\"", refs[&i]).unwrap();
            }
            El::Link { label, .. } => writeln!(out, "[{}] link \"{label}\"", refs[&i]).unwrap(),
            El::Code(t) => writeln!(out, "[{}] code \"{t}\"", refs[&i]).unwrap(),
            El::Input { .. } => {}
        }
    }
    out
}

#[async_trait]
impl BrowserDriver for SiteBrowser {
    async fn open(&self, url: &str, _profile: Option<&str>) -> Result<String, String> {
        let url = Url::parse(url).map_err(|e| e.to_string())?;
        self.load("GET", &url, None).await
    }

    async fn snapshot(&self, _interactive_only: bool) -> Result<String, String> {
        let st = self.state.lock();
        let page = st.page.as_ref().ok_or("no page is open")?;
        Ok(render(page, &st.typed))
    }

    async fn click(&self, r: &str) -> Result<String, String> {
        let (el, page) = self.element(r)?;
        match el {
            El::Link { href, .. } => {
                let url = self.resolve(&href)?;
                self.load("GET", &url, None).await
            }
            El::Button { form: Some(f), .. } => {
                let action = self.resolve(&page.actions[f])?;
                let typed = self.state.lock().typed.clone();
                let body = {
                    let mut body = url::form_urlencoded::Serializer::new(String::new());
                    for (key, e) in interactive_all(&page) {
                        if let El::Input {
                            name,
                            kind,
                            value,
                            form,
                            ..
                        } = e
                            && *form == f
                        {
                            let v = key
                                .as_ref()
                                .and_then(|k| typed.get(k))
                                .cloned()
                                .unwrap_or_else(|| value.clone());
                            if kind == "checkbox" && v.is_empty() {
                                continue;
                            }
                            body.append_pair(name, &v);
                        }
                    }
                    body.finish()
                };
                self.load("POST", &action, Some(body)).await
            }
            El::Input { kind, .. } if kind == "checkbox" => {
                let mut st = self.state.lock();
                let on = st.typed.get(r).is_some_and(|v| v == "on");
                st.typed
                    .insert(r.to_owned(), if on { String::new() } else { "on".into() });
                Ok(format!("toggled {r}"))
            }
            _ => Ok(format!("clicked {r}")),
        }
    }

    async fn fill(&self, r: &str, text: &str) -> Result<String, String> {
        match self.element(r)?.0 {
            El::Input { .. } => {
                self.state
                    .lock()
                    .typed
                    .insert(r.to_owned(), text.to_owned());
                Ok(format!("filled {r}"))
            }
            _ => Err(format!("{r} is not a field")),
        }
    }

    async fn field_target(&self, r: &str) -> Result<FieldTarget, String> {
        let El::Input { label, kind, .. } = self.element(r)?.0 else {
            return Err(format!("{r} is not a field"));
        };
        let origin = self.origin();
        Ok(FieldTarget {
            name: label,
            role: "textbox".into(),
            frame_origin: origin.clone(),
            top_origin: origin,
            sensitive: kind == "password",
        })
    }

    async fn fill_secret(
        &self,
        r: &str,
        text: &str,
        accept: &(dyn Fn(Option<String>) -> bool + Send + Sync),
    ) -> Result<String, String> {
        let El::Input { .. } = self.element(r)?.0 else {
            return Err(format!("{r} is not a field"));
        };
        if !accept(self.origin()) {
            return Err("refused: the field is not on the site this secret belongs to".into());
        }
        self.state
            .lock()
            .typed
            .insert(r.to_owned(), text.to_owned());
        Ok(format!("filled {r}"))
    }

    async fn read_value(
        &self,
        target: ReadTarget<'_>,
        accept: &(dyn Fn(Option<String>, Option<String>) -> bool + Send + Sync),
    ) -> Result<ReadValue, String> {
        let ReadTarget::Ref(r) = target else {
            return Err("this browser reads by ref only".into());
        };
        let (value, name, role) = match self.element(r)?.0 {
            El::Code(t) => (t, "Setup key".to_owned(), "code".to_owned()),
            El::Input { value, label, .. } => (value, label, "textbox".to_owned()),
            _ => return Err(format!("{r} holds no value")),
        };
        let origin = self.origin();
        if !accept(origin.clone(), origin.clone()) {
            return Err("refused: the element is not on the account's own site".into());
        }
        Ok(ReadValue {
            value,
            name,
            role,
            frame_origin: origin.clone(),
            top_origin: origin,
        })
    }

    async fn select(&self, _: &str, _: &str) -> Result<String, String> {
        Err("no dropdowns on these sites".into())
    }

    async fn press(&self, key: &str) -> Result<String, String> {
        Ok(format!("pressed {key}"))
    }

    async fn scroll(&self, _: Option<&str>, _: f64, _: f64) -> Result<String, String> {
        Ok("scrolled".into())
    }

    async fn wait(&self, _: Wait, _: Option<Duration>) -> Result<String, String> {
        Ok("waited".into())
    }

    async fn back(&self) -> Result<String, String> {
        let url = self
            .state
            .lock()
            .history
            .pop()
            .ok_or("nothing to go back to")?;
        self.load("GET", &url, None).await
    }

    async fn tabs(&self, _: Option<usize>) -> Result<String, String> {
        Ok("1 tab".into())
    }

    async fn screenshot(
        &self,
        _: bool,
        _: Option<&str>,
    ) -> Result<(Vec<u8>, &'static str), String> {
        Ok((vec![0xFF, 0xD8, 0xFF], "jpg"))
    }

    async fn eval(&self, _: &str) -> Result<Value, String> {
        Ok(Value::Null)
    }

    async fn current_url(&self) -> Option<String> {
        self.state.lock().page.as_ref().map(|p| p.url.to_string())
    }

    async fn current_tab_id(&self) -> Option<String> {
        Some("tab-1".into())
    }
}

/// Every element with its ref (`None` for hidden inputs).
fn interactive_all(p: &Page) -> Vec<(Option<String>, &El)> {
    let refs = interactive(p);
    p.els
        .iter()
        .map(|e| {
            let key = refs
                .iter()
                .find(|(_, r)| std::ptr::eq(*r, e))
                .map(|(k, _)| k.clone());
            (key, e)
        })
        .collect()
}
